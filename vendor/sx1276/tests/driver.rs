//! Driver behaviour against mocked SPI / GPIO (embedded-hal-mock, eh1).
use embedded_hal_mock::eh1::delay::NoopDelay;
use embedded_hal_mock::eh1::digital::{Mock as Pin, State, Transaction as PinT};
use embedded_hal_mock::eh1::spi::{Mock as Spi, Transaction as T};
use sx1276::*;

/// One SpiDevice transaction made of a single full-duplex transfer.
fn xfer(out: &[u8], inp: &[u8]) -> Vec<T<u8>> {
    vec![
        T::transaction_start(),
        T::transfer_in_place(out.to_vec(), inp.to_vec()),
        T::transaction_end(),
    ]
}
fn wr(addr: u8, val: u8) -> Vec<T<u8>> {
    xfer(&[addr | 0x80, val], &[0, 0])
}
fn rd(addr: u8, val: u8) -> Vec<T<u8>> {
    xfer(&[addr, 0], &[0, val])
}

type Radio = Sx1276<Spi<u8>, Pin, PolledIrq<Pin, NoopDelay>, NoopDelay>;

fn radio(spi: Vec<T<u8>>, rst: &[PinT], dio0: &[PinT]) -> Radio {
    Sx1276::new(
        Spi::new(&spi),
        Pin::new(rst),
        PolledIrq::new(Pin::new(dio0), NoopDelay, 100),
        NoopDelay,
    )
}

fn done(r: Radio) {
    let (mut spi, mut rst, irq, _) = r.release();
    let (mut dio0, _) = irq.release();
    spi.done();
    rst.done();
    dio0.done();
}

#[test]
fn reset_and_version() {
    let mut r = radio(
        rd(0x42, 0x12),
        &[PinT::set(State::Low), PinT::set(State::High)],
        &[],
    );
    r.reset().unwrap();
    assert_eq!(r.version().unwrap(), 0x12);
    done(r);
}

#[test]
fn css_mode_and_frequency_burst() {
    let mut s = vec![];
    s.extend(rd(0x01, 0x09)); // FSK standby, LF bit set after reset
    s.extend(wr(0x01, 0x08)); // FSK sleep (keeps LF)
    s.extend(wr(0x01, 0x88)); // CSS mode sleep
    s.extend(xfer(&[0x86, 0xE4, 0xC0, 0x00], &[0; 4])); // RegFrf burst
    s.extend(wr(0x01, 0x80)); // HF page selected
    s.extend(xfer(&[0x06, 0, 0, 0], &[0, 0xE4, 0xC0, 0x00]));
    let mut r = radio(s, &[], &[]);
    r.enter_css_mode().unwrap();
    r.set_frequency(915_000_000).unwrap();
    assert_eq!(r.frequency().unwrap(), 915_000_000);
    assert_eq!(
        r.set_frequency(100_000_000),
        Err(Error::Config(ConfigError::FrequencyOutOfRange))
    );
    done(r);
}

#[test]
fn configure_is_one_burst_plus_extras() {
    let c = CssConfig::default();
    let mut s = vec![];
    s.extend(xfer(
        &[0x9D, 0x72, 0x74, 0x64, 0x00, 0x08, 0xFF, 0xFF, 0x00],
        &[0; 9],
    ));
    s.extend(wr(0x26, 0x04));
    s.extend(rd(0x31, 0xC3));
    s.extend(wr(0x31, 0xC3));
    s.extend(wr(0x37, 0x0A));
    s.extend(wr(0x39, 0x12));
    let mut r = radio(s, &[], &[]);
    r.configure(&c).unwrap();
    done(r);
}

#[test]
fn transmit_sequence() {
    let payload = b"hello";
    let mut s = vec![];
    s.extend(wr(0x01, 0x81)); // standby
    s.extend(wr(0x0D, 0x00)); // FifoAddrPtr
    let mut fifo = vec![0x80];
    fifo.extend_from_slice(payload);
    s.extend(xfer(&fifo, &[0; 6])); // whole payload in one transaction
    s.extend(wr(0x22, 5));
    s.extend(rd(0x40, 0x00));
    s.extend(wr(0x40, 0x40)); // DIO0 = TxDone
    s.extend(wr(0x01, 0x83)); // TX
    s.extend(rd(0x12, 0x08));
    s.extend(wr(0x12, 0x08));
    let mut r = radio(s, &[], &[PinT::get(State::Low), PinT::get(State::High)]);
    r.transmit(payload, 1_000).unwrap();
    done(r);
}

#[test]
fn receive_packet_four_transactions() {
    let mut s = vec![];
    // start_receive: standby, FifoAddrPtr, DIO map (read+write), clear, RXCONT
    s.extend(wr(0x01, 0x81));
    s.extend(wr(0x0D, 0x00));
    s.extend(rd(0x40, 0x80));
    s.extend(wr(0x40, 0x00));
    s.extend(wr(0x12, 0xFF));
    s.extend(wr(0x01, 0x85));
    // read_packet: status burst 0x10..=0x1A
    let mut st = vec![0u8; 12];
    st[1] = 0x20; // FifoRxCurrentAddr
    st[3] = 0x50; // RxDone | ValidHeader
    st[4] = 3; // RxNbBytes
    st[10] = 0xF8; // SNR -2 dB
    st[11] = 70; // PktRssi
    s.extend(xfer(&[0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &st));
    s.extend(wr(0x0D, 0x20));
    s.extend(xfer(&[0x00, 0, 0, 0], &[0, b'a', b'b', b'c']));
    s.extend(wr(0x12, 0x50));
    let mut r = radio(s, &[], &[PinT::get(State::High)]);
    r.start_receive(RxMode::Continuous).unwrap();
    let mut buf = [0u8; 255];
    let p = r.receive(&mut buf, 1_000).unwrap().unwrap();
    assert_eq!(&buf[..p.len], b"abc");
    assert_eq!(p.snr_quarter_db, -8);
    assert_eq!(p.snr_db(), -2.0);
    assert_eq!(p.rssi_dbm, -157 + 70 - 2);
    done(r);
}

#[test]
fn receive_crc_error_is_reported_and_cleared() {
    let mut s = vec![];
    let mut st = vec![0u8; 12];
    st[3] = 0x70;
    st[4] = 9;
    s.extend(xfer(&[0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &st));
    s.extend(wr(0x12, 0x70));
    let mut r = radio(s, &[], &[]);
    let mut buf = [0u8; 16];
    assert_eq!(r.read_packet(&mut buf), Err(Error::Crc));
    done(r);
}

#[test]
fn no_packet_costs_one_transaction() {
    let s = xfer(&[0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[0; 12]);
    let mut r = radio(s, &[], &[]);
    let mut buf = [0u8; 16];
    assert_eq!(r.read_packet(&mut buf), Ok(None));
    done(r);
}

#[test]
fn cad_uses_dio0() {
    let mut s = vec![];
    s.extend(wr(0x01, 0x81));
    s.extend(rd(0x40, 0x00));
    s.extend(wr(0x40, 0x80));
    s.extend(wr(0x12, 0xFF));
    s.extend(wr(0x01, 0x87));
    s.extend(rd(0x12, 0x04));
    s.extend(wr(0x12, 0x05));
    let mut r = radio(s, &[], &[PinT::get(State::Low), PinT::get(State::High)]);
    assert!(!r.cad(10_000).unwrap());
    done(r);
}

#[test]
fn payload_bounds() {
    let mut r = radio(vec![], &[], &[]);
    assert_eq!(r.start_transmit(&[]), Err(Error::PayloadSize));
    assert_eq!(r.start_transmit(&[0u8; 256]), Err(Error::PayloadSize));
    done(r);
}

/// A long FSK packet first seen at PayloadReady (its head already overrun) is an error,
/// and the half-read state is cleared instead of carrying into the next packet.
#[test]
fn fsk_long_packet_first_seen_at_payload_ready_resets() {
    let mut s = vec![];
    s.extend(rd(0x3F, 0x04 | 0x02)); // PayloadReady, CrcOk, FIFO not empty
    s.extend(rd(0x11, 0x80)); // RSSI
    s.extend(rd(0x00, 200)); // length byte: 200 more bytes, more than the FIFO holds
    s.extend(rd(0x3F, 0x40)); // FIFO empty
    let mut r = radio(s, &[], &[]);
    let mut st = FskLongRx::default();
    assert!(matches!(
        r.fsk_poll_long(&mut st, 0, 1000),
        Err(Error::PayloadSize)
    ));
    assert!(!st.in_progress());
    done(r);
}

/// CRC auto-clear off sets CrcAutoClearOff and makes reads check CrcOk; the default
/// leaves the register as the measured runs had it.
#[test]
fn fsk_crc_auto_clear_off_is_opt_in() {
    let off = fsk::FskConfig {
        crc_auto_clear: false,
        ..Default::default()
    };
    assert_eq!(off.packet_config1(), 0x80 | 0x40 | 0x10 | 0x08);
    assert!(off.checks_crc_on_read());
    let def = fsk::FskConfig::default();
    assert_eq!(def.packet_config1(), 0x80 | 0x40 | 0x10);
    assert!(!def.checks_crc_on_read());
    let no_crc = fsk::FskConfig { crc: false, ..off };
    assert_eq!(no_crc.packet_config1() & 0x18, 0);
}
