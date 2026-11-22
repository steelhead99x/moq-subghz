//! RP2040-based device link-test firmware: the onboard RFM95W (SX1276) driven by the
//! `sx1276` crate, controlled over USB CDC serial with a line protocol.
//!
//! Wiring (from the RP2040-based device KiCad netlist): SPI0 MISO=GPIO16, NSS=GPIO17, SCK=GPIO18,
//! MOSI=GPIO19, RESET=GPIO21, DIO0=GPIO10, RX LED=GPIO25. 12 MHz crystal, W25Q16 flash.
//!
//! Commands (one per line):
//!   L <freq_hz> <sf> <bw_hz> <cr_den> <dbm> <implicit_len|0>   LoRa config (CRC on, sync 0x12, preamble 8)
//!   F <freq_hz> <bitrate> <fdev_hz> <dbm> [preamble_bytes] [long] [force]  FSK config (CRC, whitening, default 4B preamble);
//!                long=1: receive frames > 63 B by polling the FIFO (TX of > 63 B always refills the FIFO)
//!   R            continuous receive; each packet -> "R <t_us> <rssi> <snr_q> <hex>"
//!   Q <hex>      queue a packet; queued packets are sent back to back ("B <t_us>" at start, "D <t_us> <n>" when drained)
//!   S            standby + stats      I   info      BOOT   reboot into the RP2040 USB bootloader
//!   X            re-initialise the radio: RESET pulse, retry until RegVersion reads 0x12 ->
//!                "X sx_version=0x12 tries=<n>"
//! The radio RESET line is pulsed at boot (with retries), so `I` answers `sx_version=0x12`
//! right after power-up. Before 0.5.1 the line was left low (chip held in reset) until the
//! first `L` or `F`, so an early `I` read 0x00.
//! Opening the port at 1200 baud and dropping DTR also reboots into the bootloader.
//!
//! `L` and `F` refuse (`Some(Refused)`, radio left in standby) a carrier outside 902-928 MHz
//! or within 0.3 MHz of 925.0, power outside 2..=5 dBm, and for `L` a bandwidth, spreading
//! factor or coding rate the radio doesn't have: the same limits the host tools hold
//! themselves to. A bad `Q` frame is answered `ERR tx bad-hex`, and USB input waits while
//! the queue is full instead of dropping a frame. The shipped `firmware/catwan-fw-v5.uf2`
//! (0.5.0, measured) and `catwan-fw-0.5.1.uf2` predate these checks; they take a new image
//! and a reflash.
#![no_std]
#![no_main]

use core::fmt::Write as _;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::OutputPin;
use embedded_hal_bus::spi::ExclusiveDevice;
use hal::{
    clocks::init_clocks_and_plls, fugit::RateExtU32, pac, usb::UsbBus, Clock, Sio, Watchdog,
};
use heapless::{Deque, String, Vec};
use panic_halt as _;
use rp2040_hal as hal;
use sx1276::{config::*, fsk::FskConfig, IrqLine, PolledIrq, RxMode, Sx1276};
use usb_device::{class_prelude::UsbBusAllocator, prelude::*};
use usbd_serial::SerialPort;

#[link_section = ".boot2"]
#[used]
pub static BOOT2_FIRMWARE: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const XOSC_HZ: u32 = 12_000_000;

/// The band and power the host tools hold themselves to, enforced here too so a raw serial
/// session can't key the radio outside them.
fn tx_allowed(freq_hz: u32, dbm: u32) -> bool {
    (902_000_000..=928_000_000).contains(&freq_hz)
        && freq_hz.abs_diff(925_000_000) >= 300_000
        && (2..=5).contains(&dbm)
}
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Pulse the radio RESET line and read `RegVersion` until it answers 0x12 (SX1276).
/// RESET is active low. The pin is an output that starts low, so without a pulse the chip
/// stays in reset and every register reads 0x00. Each try holds RESET low a little longer
/// and waits a little longer after release. Returns (last version read, tries used).
macro_rules! bring_up {
    ($radio:expr, $timer:expr) => {{
        let mut v = 0u8;
        let mut tries = 0u8;
        while tries < 5 {
            tries += 1;
            let _ = $radio.reset();
            $timer.delay_ms(5 * tries as u32);
            v = $radio.version().unwrap_or(0);
            if v == 0x12 {
                break;
            }
        }
        (v, tries)
    }};
}

#[derive(Copy, Clone, PartialEq)]
enum Modem {
    Lora,
    Fsk,
}
#[derive(Copy, Clone, PartialEq)]
enum State {
    Idle,
    Rx,
    Tx,
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
fn unhex(s: &[u8], out: &mut Vec<u8, 255>) -> bool {
    if !s.len().is_multiple_of(2) || s.len() / 2 > 255 {
        return false;
    }
    for p in s.chunks(2) {
        match (hexval(p[0]), hexval(p[1])) {
            (Some(a), Some(b)) => {
                let _ = out.push(a << 4 | b);
            }
            _ => return false,
        }
    }
    true
}

/// Table-driven hex: core::fmt's `{:02x}` costs microseconds per byte on the M0+, so a
/// 255-byte frame took long enough to format that the next back-to-back FSK packet could
/// overrun the 64-byte FIFO before the radio was polled again.
const HEX: &[u8; 16] = b"0123456789abcdef";
fn hex_into<const N: usize>(s: &mut String<N>, data: &[u8]) {
    for &b in data {
        let _ = s.push(HEX[(b >> 4) as usize] as char);
        let _ = s.push(HEX[(b & 15) as usize] as char);
    }
}

struct Out(Deque<u8, 16384>, u32);
impl Out {
    fn line(&mut self, s: &[u8]) {
        if self.0.capacity() - self.0.len() < s.len() + 1 {
            self.1 += 1;
            return;
        }
        for &b in s {
            let _ = self.0.push_back(b);
        }
        let _ = self.0.push_back(b'\n');
    }
}

#[hal::entry]
fn main() -> ! {
    let mut pac = pac::Peripherals::take().unwrap();
    let mut watchdog = Watchdog::new(pac.WATCHDOG);
    let clocks = init_clocks_and_plls(
        XOSC_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .ok()
    .unwrap();
    let timer = hal::Timer::new(pac.TIMER, &mut pac.RESETS, &clocks);
    let sio = Sio::new(pac.SIO);
    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    let miso = pins.gpio16.into_function::<hal::gpio::FunctionSpi>();
    let sck = pins.gpio18.into_function::<hal::gpio::FunctionSpi>();
    let mosi = pins.gpio19.into_function::<hal::gpio::FunctionSpi>();
    let mut nss = pins.gpio17.into_push_pull_output();
    let _ = nss.set_high();
    let rst = pins.gpio21.into_push_pull_output();
    let dio0 = pins.gpio10.into_pull_down_input();
    let mut led = pins.gpio25.into_push_pull_output();
    let spi = hal::spi::Spi::<_, _, _, 8>::new(pac.SPI0, (mosi, miso, sck)).init(
        &mut pac.RESETS,
        clocks.peripheral_clock.freq(),
        10u32.MHz(),
        embedded_hal::spi::MODE_0,
    );
    let dev = ExclusiveDevice::new(spi, nss, timer).unwrap();
    let mut radio = Sx1276::new(dev, rst, PolledIrq::new(dio0, timer, 5), timer);
    let mut boot_timer = timer;
    let _ = bring_up!(radio, boot_timer);

    let usb_bus = UsbBusAllocator::new(UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    ));
    let mut serial = SerialPort::new(&usb_bus);
    let mut usb = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x2e8a, 0x000a))
        .strings(&[StringDescriptors::default()
            .manufacturer("kixel sx1276-rs")
            .product("RP2040-based device")
            .serial_number("613863E6354E26A3")])
        .unwrap()
        .device_class(usbd_serial::USB_CLASS_CDC)
        .build();

    let mut out = Out(Deque::new(), 0);
    let mut line: Vec<u8, 600> = Vec::new();
    let mut pend = [0u8; 64];
    let (mut pend_pos, mut pend_len) = (0usize, 0usize);
    let mut txq: Deque<Vec<u8, 255>, 12> = Deque::new();
    let mut state = State::Idle;
    let mut modem = Modem::Lora;
    let mut ready = false;
    let mut tx_busy = false;
    let mut tx_started = 0u64;
    let mut tx_pkt_t = 0u64; // when the packet now on air was started (per-packet timeout)
    let mut tx_n = 0u32;
    let (mut rx_ok, mut rx_err) = (0u32, 0u32);
    let mut buf = [0u8; 255];
    let now = || timer.get_counter().ticks();
    let mut s: String<620> = String::new();
    // FSK long-packet mode (> 63 B frames): FIFO refill on TX, polled FIFO drain on RX
    let mut fsk_long = false;
    let mut fsk_stall = 1000u32;
    let mut lst = sx1276::FskLongRx::default();

    loop {
        // `poll` drives the USB state machine. The bool is "still busy"; the loop
        // always continues either way.
        let _ = usb.poll(&mut [&mut serial]);
        // 1200-baud touch -> bootloader
        if serial.line_coding().data_rate() == 1200 && !serial.dtr() {
            hal::rom_data::reset_to_usb_boot(1 << 25, 0);
        }
        // input: read more once the last read is used up. A Q line blocked on a full queue
        // holds the rest of its read, so this is the only backpressure needed, and S still
        // gets through when the queue is full but not draining.
        if pend_pos == pend_len {
            if let Ok(n) = serial.read(&mut pend) {
                (pend_pos, pend_len) = (0, n);
            }
        }
        while pend_pos < pend_len {
            let b = pend[pend_pos];
            // A Q line runs only when its packet has a slot; until then the rest of the
            // read waits here, so a batch of short Q lines can't overrun the queue.
            if (b == b'\n' || b == b'\r') && line.starts_with(b"Q ") && txq.is_full() {
                break;
            }
            pend_pos += 1;
            if b == b'\n' || b == b'\r' {
                if !line.is_empty() {
                    // execute
                    let mut it = line.split(|&c| c == b' ').filter(|t| !t.is_empty());
                    let cmd = it.next().unwrap_or(b"");
                    let mut num = || -> u32 {
                        it.next()
                            .and_then(|t| core::str::from_utf8(t).ok())
                            .and_then(|t| t.parse().ok())
                            .unwrap_or(0)
                    };
                    s.clear();
                    match cmd {
                        b"L" => {
                            let (f, sf, bw, cr, dbm, il) =
                                (num(), num(), num(), num(), num(), num());
                            let bwe = match bw {
                                125_000 => Some(Bandwidth::Bw125kHz),
                                250_000 => Some(Bandwidth::Bw250kHz),
                                500_000 => Some(Bandwidth::Bw500kHz),
                                _ => None,
                            };
                            let sfe = SpreadingFactor::from_u8(sf as u8);
                            let cre = CodingRate::from_denominator(cr as u8);
                            match (bwe, sfe, cre) {
                                (Some(bandwidth), Some(spreading_factor), Some(coding_rate))
                                    if tx_allowed(f, dbm) && il <= 255 =>
                                {
                                    let cfg = LoRaConfig {
                                        bandwidth,
                                        spreading_factor,
                                        coding_rate,
                                        header: if il > 0 {
                                            HeaderMode::Implicit {
                                                payload_len: il as u8,
                                            }
                                        } else {
                                            HeaderMode::Explicit
                                        },
                                        ..LoRaConfig::default()
                                    };
                                    let mut r0 = radio.init(f, &cfg);
                                    if matches!(r0, Err(sx1276::Error::BadVersion(_))) {
                                        let _ = bring_up!(radio, boot_timer);
                                        r0 = radio.init(f, &cfg);
                                    }
                                    let r = r0.and_then(|_| {
                                        radio.set_tx_power(PaOutput::PaBoost, dbm as i8)
                                    });
                                    modem = Modem::Lora;
                                    let _ = write!(s, "OK L {} {:?}", f, r.err());
                                }
                                _ => {
                                    let _ = radio.standby();
                                    let _ = write!(s, "OK L {} Some(Refused)", f);
                                }
                            }
                            state = State::Idle;
                        }
                        b"F" => {
                            let (f, br, fd, dbm, pre, lg, force) =
                                (num(), num(), num(), num(), num(), num(), num());
                            if !tx_allowed(f, dbm) {
                                let _ = radio.standby();
                                state = State::Idle;
                                let _ = write!(s, "OK F {} {} {} Some(Refused)", f, br, fd);
                                out.line(s.as_bytes());
                                s.clear();
                                line.clear();
                                continue;
                            }
                            fsk_long = lg != 0;
                            fsk_stall = 36 * 8_000_000 / br.max(1);
                            let cfg = FskConfig {
                                bitrate_bps: br,
                                fdev_hz: fd,
                                preamble_bytes: if pre == 0 { 4 } else { pre as u16 },
                                unchecked: force != 0,
                                // long mode drains the FIFO mid-packet: keep failed packets
                                crc_auto_clear: !fsk_long,
                                ..FskConfig::default()
                            };
                            let mut r0 = radio.init(f, &LoRaConfig::default());
                            if matches!(r0, Err(sx1276::Error::BadVersion(_))) {
                                let _ = bring_up!(radio, boot_timer);
                                r0 = radio.init(f, &LoRaConfig::default());
                            }
                            let r = r0
                                .and_then(|_| radio.enter_fsk_mode())
                                .and_then(|_| radio.set_frequency(f))
                                .and_then(|_| radio.write_raw(0x0C, 0x23)) // LNA max gain + HF boost
                                .and_then(|_| radio.configure_fsk(&cfg))
                                .and_then(|_| radio.set_tx_power(PaOutput::PaBoost, dbm as i8))
                                .and_then(|_| radio.standby());
                            modem = Modem::Fsk;
                            state = State::Idle;
                            let _ = write!(s, "OK F {} {} {} {:?}", f, br, fd, r.err());
                        }
                        b"R" => {
                            let r = if modem == Modem::Lora {
                                radio.start_receive(RxMode::Continuous)
                            } else {
                                radio.fsk_start_receive()
                            };
                            state = State::Rx;
                            rx_ok = 0;
                            rx_err = 0;
                            let _ = write!(s, "OK R {:?}", r.err());
                        }
                        b"Q" => {
                            let mut p: Vec<u8, 255> = Vec::new();
                            // Every rejected frame is reported; the sender counts `ERR tx` lines.
                            let ok = it.next().is_some_and(|h| unhex(h, &mut p)) && !p.is_empty();
                            if !ok {
                                let _ = write!(s, "ERR tx bad-hex");
                            } else if txq.push_back(p).is_err() {
                                let _ = write!(s, "ERR tx queue-full");
                            }
                            if state != State::Tx {
                                state = State::Tx;
                                tx_n = 0;
                            }
                        }
                        b"S" => {
                            let _ = radio.standby();
                            state = State::Idle;
                            txq.clear();
                            tx_busy = false;
                            let _ = write!(
                                s,
                                "OK S rx_ok={} rx_err={} tx={} out_drop={}",
                                rx_ok, rx_err, tx_n, out.1
                            );
                        }
                        b"I" => {
                            let v = radio.version().unwrap_or(0);
                            let _ = write!(
                                s,
                                "I catwan-fw {} sx_version=0x{:02x} t_us={}",
                                VERSION,
                                v,
                                now()
                            );
                        }
                        b"X" => {
                            let (v, tries) = bring_up!(radio, boot_timer);
                            state = State::Idle;
                            txq.clear();
                            tx_busy = false;
                            let _ = write!(s, "X sx_version=0x{:02x} tries={}", v, tries);
                        }
                        b"BOOT" => hal::rom_data::reset_to_usb_boot(1 << 25, 0),
                        _ => {
                            let _ = write!(s, "ERR");
                        }
                    }
                    if !s.is_empty() {
                        out.line(s.as_bytes());
                    }
                    line.clear();
                }
            } else if line.push(b).is_err() {
                line.clear();
            }
            let _ = ready;
        }
        // radio
        match state {
            State::Rx => {
                if modem == Modem::Fsk && fsk_long {
                    let t = now();
                    let r = match radio.fsk_poll_long(&mut lst, t as u32, fsk_stall) {
                        Ok(Some(p)) => {
                            buf[..p.len].copy_from_slice(lst.payload());
                            Ok(Some(p))
                        }
                        x => x,
                    };
                    s.clear();
                    match r {
                        Ok(Some(p)) => {
                            rx_ok += 1;
                            let _ = write!(s, "R {} {} {} ", t, p.rssi_dbm, p.snr_quarter_db);
                            hex_into(&mut s, &buf[..p.len]);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            rx_err += 1;
                            let _ = write!(s, "X {} {:?}", t, e);
                        }
                    }
                    if !s.is_empty() {
                        out.line(s.as_bytes());
                    }
                } else if radio.irq_line().is_asserted().unwrap_or(false) {
                    let t = now();
                    let r = if modem == Modem::Lora {
                        radio.read_packet(&mut buf)
                    } else {
                        radio.fsk_read_packet(&mut buf)
                    };
                    s.clear();
                    match r {
                        Ok(Some(p)) => {
                            rx_ok += 1;
                            let _ = led.set_high();
                            let _ = write!(s, "R {} {} {} ", t, p.rssi_dbm, p.snr_quarter_db);
                            hex_into(&mut s, &buf[..p.len]);
                            let _ = led.set_low();
                        }
                        Ok(None) => {}
                        Err(e) => {
                            rx_err += 1;
                            let _ = write!(s, "X {} {:?}", t, e);
                        }
                    }
                    if !s.is_empty() {
                        out.line(s.as_bytes());
                    }
                }
            }
            State::Tx => {
                if tx_busy {
                    if radio.irq_line().is_asserted().unwrap_or(false) {
                        let _ = if modem == Modem::Lora {
                            radio.finish_transmit()
                        } else {
                            radio.fsk_finish_transmit()
                        };
                        tx_busy = false;
                        tx_n += 1;
                        if txq.is_empty() {
                            s.clear();
                            let _ = write!(s, "D {} {} {}", now(), tx_n, now() - tx_started);
                            out.line(s.as_bytes());
                        }
                    } else if now() - tx_pkt_t > 2_000_000 {
                        // v5: per-packet timeout. v4 timed out 60 s after the *burst* started, so a
                        // TX stream longer than a minute began each packet on top of the last one.
                        let _ = radio.standby();
                        tx_busy = false;
                        out.line(b"ERR txtimeout");
                    }
                }
                if !tx_busy {
                    if let Some(p) = txq.pop_front() {
                        if tx_n == 0 {
                            tx_started = now();
                            s.clear();
                            let _ = write!(s, "B {}", tx_started);
                            out.line(s.as_bytes());
                        }
                        tx_pkt_t = now();
                        let r = if modem == Modem::Lora {
                            radio.start_transmit(&p)
                        } else if p.len() > 63 {
                            radio.fsk_transmit_long(&p, 1_000_000)
                        } else {
                            radio.fsk_start_transmit(&p)
                        };
                        if r.is_ok() {
                            tx_busy = true;
                        } else {
                            out.line(b"ERR tx");
                        }
                    }
                }
            }
            State::Idle => {}
        }
        // flush output
        while !out.0.is_empty() {
            let (a, _) = out.0.as_slices();
            let n = a.len().min(64);
            let mut chunk = [0u8; 64];
            chunk[..n].copy_from_slice(&a[..n]);
            match serial.write(&chunk[..n]) {
                Ok(w) => {
                    for _ in 0..w {
                        out.0.pop_front();
                    }
                    if w < n {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        ready = true;
    }
}
