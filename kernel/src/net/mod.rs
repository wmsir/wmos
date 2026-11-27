use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use crate::arch::{cpu, pit};
use crate::drivers::e1000;
use crate::task;

const ETH_TYPE_ARP: u16 = 0x0806;
const ETH_TYPE_IPV4: u16 = 0x0800;
const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_UDP: u8 = 17;
const ICMP_ECHO_ID: u16 = 0x574D;

const ARP_CACHE_SIZE: usize = 16;
const MIN_ETH_LEN: usize = 60;

/// Active static IPv4 network configuration.
#[derive(Clone, Copy)]
pub struct NetConfig {
    pub ip: [u8; 4],
    pub netmask: [u8; 4],
    pub gateway: [u8; 4],
}

#[derive(Clone, Copy)]
struct ArpEntry {
    valid: bool,
    ip: [u8; 4],
    mac: [u8; 6],
}

impl ArpEntry {
    const fn empty() -> Self {
        Self {
            valid: false,
            ip: [0; 4],
            mac: [0; 6],
        }
    }
}

struct NetState {
    up: bool,
    local_mac: [u8; 6],
    cfg: NetConfig,
    arp_cache: [ArpEntry; ARP_CACHE_SIZE],
    arp_next_slot: usize,
    last_ping_reply: Option<([u8; 4], u16, u16)>,
}

struct NetLock {
    locked: AtomicBool,
    inner: UnsafeCell<NetState>,
}

unsafe impl Sync for NetLock {}

static NET: NetLock = NetLock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(NetState {
        up: false,
        local_mac: [0; 6],
        cfg: NetConfig {
            ip: [10, 0, 2, 15],
            netmask: [255, 255, 255, 0],
            gateway: [10, 0, 2, 2],
        },
        arp_cache: [ArpEntry::empty(); ARP_CACHE_SIZE],
        arp_next_slot: 0,
        last_ping_reply: None,
    }),
};

static IP_IDENT: AtomicU16 = AtomicU16::new(1);

struct IrqGuard<'a> {
    lock: &'a NetLock,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a NetLock) -> Self {
        let rflags: u64;
        unsafe {
            asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags));
        }
        let saved_if = (rflags & 0x200) != 0;
        cpu::cli();
        while lock
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self { lock, saved_if }
    }

    #[inline]
    fn state_mut(&mut self) -> &mut NetState {
        unsafe { &mut *self.lock.inner.get() }
    }
}

impl Drop for IrqGuard<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.saved_if {
            cpu::sti();
        }
    }
}

fn rfc1071_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

#[inline]
fn same_subnet(a: [u8; 4], cfg: NetConfig) -> bool {
    for i in 0..4 {
        if (a[i] & cfg.netmask[i]) != (cfg.ip[i] & cfg.netmask[i]) {
            return false;
        }
    }
    true
}

fn arp_learn(ip: [u8; 4], mac: [u8; 6]) {
    let mut guard = IrqGuard::acquire(&NET);
    let st = guard.state_mut();
    for entry in &mut st.arp_cache {
        if entry.valid && entry.ip == ip {
            entry.mac = mac;
            return;
        }
    }
    let slot = st.arp_next_slot;
    st.arp_cache[slot] = ArpEntry {
        valid: true,
        ip,
        mac,
    };
    st.arp_next_slot = (slot + 1) % ARP_CACHE_SIZE;
}

fn arp_lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    let mut guard = IrqGuard::acquire(&NET);
    let st = guard.state_mut();
    st.arp_cache
        .iter()
        .find(|e| e.valid && e.ip == ip)
        .map(|e| e.mac)
}

fn send_arp_packet(
    dst_mac: [u8; 6],
    oper: u16,
    target_mac: [u8; 6],
    target_ip: [u8; 4],
) -> Result<(), &'static str> {
    let (local_mac, local_ip) = {
        let mut guard = IrqGuard::acquire(&NET);
        let st = guard.state_mut();
        (st.local_mac, st.cfg.ip)
    };

    let mut frame = [0u8; MIN_ETH_LEN];
    frame[0..6].copy_from_slice(&dst_mac);
    frame[6..12].copy_from_slice(&local_mac);
    frame[12..14].copy_from_slice(&ETH_TYPE_ARP.to_be_bytes());

    let arp = &mut frame[14..42];
    arp[0..2].copy_from_slice(&1u16.to_be_bytes()); // Ethernet
    arp[2..4].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
    arp[4] = 6;
    arp[5] = 4;
    arp[6..8].copy_from_slice(&oper.to_be_bytes());
    arp[8..14].copy_from_slice(&local_mac);
    arp[14..18].copy_from_slice(&local_ip);
    arp[18..24].copy_from_slice(&target_mac);
    arp[24..28].copy_from_slice(&target_ip);

    e1000::send_frame(&frame)
}

fn send_ipv4_packet(
    dst_mac: [u8; 6],
    dst_ip: [u8; 4],
    proto: u8,
    payload: &[u8],
) -> Result<(), &'static str> {
    let total_ip_len = 20 + payload.len();
    let frame_len = (14 + total_ip_len).max(MIN_ETH_LEN);
    if frame_len > 1514 {
        return Err("IPv4 packet exceeds Ethernet MTU");
    }

    let (local_mac, local_ip) = {
        let mut guard = IrqGuard::acquire(&NET);
        let st = guard.state_mut();
        (st.local_mac, st.cfg.ip)
    };

    let mut frame = [0u8; 1514];
    frame[0..6].copy_from_slice(&dst_mac);
    frame[6..12].copy_from_slice(&local_mac);
    frame[12..14].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());

    let ident = IP_IDENT.fetch_add(1, Ordering::Relaxed);
    let ip = &mut frame[14..34];
    ip[0] = 0x45;
    ip[1] = 0x00;
    ip[2..4].copy_from_slice(&(total_ip_len as u16).to_be_bytes());
    ip[4..6].copy_from_slice(&ident.to_be_bytes());
    ip[6..8].copy_from_slice(&0x4000u16.to_be_bytes());
    ip[8] = 64;
    ip[9] = proto;
    ip[10..12].copy_from_slice(&[0, 0]);
    ip[12..16].copy_from_slice(&local_ip);
    ip[16..20].copy_from_slice(&dst_ip);
    let csum = rfc1071_checksum(ip);
    ip[10..12].copy_from_slice(&csum.to_be_bytes());

    frame[34..34 + payload.len()].copy_from_slice(payload);
    e1000::send_frame(&frame[..frame_len])
}

fn handle_frame(frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    let eth_type = u16::from_be_bytes([frame[12], frame[13]]);

    match eth_type {
        ETH_TYPE_ARP => {
            if frame.len() < 42 {
                return;
            }
            let arp = &frame[14..42];
            let htype = u16::from_be_bytes([arp[0], arp[1]]);
            let ptype = u16::from_be_bytes([arp[2], arp[3]]);
            if htype != 1 || ptype != ETH_TYPE_IPV4 || arp[4] != 6 || arp[5] != 4 {
                return;
            }
            let oper = u16::from_be_bytes([arp[6], arp[7]]);
            let mut sha = [0u8; 6];
            let mut spa = [0u8; 4];
            let mut tpa = [0u8; 4];
            sha.copy_from_slice(&arp[8..14]);
            spa.copy_from_slice(&arp[14..18]);
            tpa.copy_from_slice(&arp[24..28]);

            arp_learn(spa, sha);

            let local_ip = config().ip;
            if oper == 1 && tpa == local_ip {
                let _ = send_arp_packet(sha, 2, sha, spa);
            }
        }
        ETH_TYPE_IPV4 => {
            if frame.len() < 34 {
                return;
            }
            let ip = &frame[14..];
            let ver = ip[0] >> 4;
            let ihl = ((ip[0] & 0x0F) as usize) * 4;
            if ver != 4 || ihl < 20 || ip.len() < ihl {
                return;
            }
            let total_len = (u16::from_be_bytes([ip[2], ip[3]]) as usize).min(ip.len());
            if total_len < ihl {
                return;
            }
            let proto = ip[9];
            let src_ip = [ip[12], ip[13], ip[14], ip[15]];
            let dst_ip = [ip[16], ip[17], ip[18], ip[19]];
            let local_ip = config().ip;
            if dst_ip != local_ip && dst_ip != [255, 255, 255, 255] {
                return;
            }

            let mut src_mac = [0u8; 6];
            src_mac.copy_from_slice(&frame[6..12]);
            arp_learn(src_ip, src_mac);

            if proto == IP_PROTO_ICMP {
                let icmp = &ip[ihl..total_len];
                if icmp.len() < 8 {
                    return;
                }
                let icmp_type = icmp[0];
                let id = u16::from_be_bytes([icmp[4], icmp[5]]);
                let seq = u16::from_be_bytes([icmp[6], icmp[7]]);

                if icmp_type == 0 {
                    let mut guard = IrqGuard::acquire(&NET);
                    guard.state_mut().last_ping_reply = Some((src_ip, id, seq));
                } else if icmp_type == 8 && icmp.len() <= 1480 {
                    let mut reply = [0u8; 1480];
                    reply[..icmp.len()].copy_from_slice(icmp);
                    reply[0] = 0; // Echo Reply
                    reply[1] = 0;
                    reply[2] = 0;
                    reply[3] = 0;
                    let csum = rfc1071_checksum(&reply[..icmp.len()]);
                    reply[2..4].copy_from_slice(&csum.to_be_bytes());
                    let _ = send_ipv4_packet(src_mac, src_ip, IP_PROTO_ICMP, &reply[..icmp.len()]);
                }
            }
        }
        _ => {}
    }
}

fn wait_step() {
    task::yield_now();
    let rflags: u64;
    unsafe {
        asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    if (rflags & 0x200) != 0 {
        cpu::hlt();
    }
}

fn resolve_mac(next_hop: [u8; 4]) -> Result<[u8; 6], &'static str> {
    if next_hop == [255, 255, 255, 255] {
        return Ok([0xFF; 6]);
    }
    if let Some(mac) = arp_lookup(next_hop) {
        return Ok(mac);
    }

    let start = pit::ticks();
    send_arp_packet([0xFF; 6], 1, [0; 6], next_hop)?;
    let mut next_retry = 33u64;

    loop {
        poll();
        if let Some(mac) = arp_lookup(next_hop) {
            return Ok(mac);
        }
        let elapsed = pit::ticks().wrapping_sub(start);
        if elapsed >= 100 {
            return Err("ARP resolution timed out");
        }
        if elapsed >= next_retry {
            let _ = send_arp_packet([0xFF; 6], 1, [0; 6], next_hop);
            next_retry += 33;
        }
        wait_step();
    }
}

/// Initializes the IPv4/ARP/ICMP/UDP network stack when an e1000 NIC is present.
pub fn init() -> bool {
    let Some(nic) = e1000::info() else {
        return false;
    };

    {
        let mut guard = IrqGuard::acquire(&NET);
        let st = guard.state_mut();
        st.up = true;
        st.local_mac = nic.mac;
        st.arp_cache = [ArpEntry::empty(); ARP_CACHE_SIZE];
        st.arp_next_slot = 0;
        st.last_ping_reply = None;
    }

    let gw = config().gateway;
    let _ = send_arp_packet([0xFF; 6], 1, [0; 6], gw);
    true
}

/// Returns `true` if the network stack is online.
pub fn is_up() -> bool {
    let mut guard = IrqGuard::acquire(&NET);
    guard.state_mut().up
}

/// Returns the active static IPv4 configuration.
pub fn config() -> NetConfig {
    let mut guard = IrqGuard::acquire(&NET);
    guard.state_mut().cfg
}

/// Drains up to 16 received Ethernet frames and handles ARP and ICMP traffic.
pub fn poll() {
    if !is_up() {
        return;
    }

    let mut rx_buf = [0u8; 2048];
    for _ in 0..16 {
        let Some(len) = e1000::poll_recv(&mut rx_buf) else {
            break;
        };
        handle_frame(&rx_buf[..len]);
    }
}

/// Sends an ICMP Echo Request to `target_ip` and waits for the matching reply, returning RTT in ms.
pub fn ping(target_ip: [u8; 4], seq: u16) -> Result<u64, &'static str> {
    if !is_up() {
        return Err("Network interface is down");
    }

    let cfg = config();
    let next_hop = if same_subnet(target_ip, cfg) {
        target_ip
    } else {
        cfg.gateway
    };
    let dst_mac = resolve_mac(next_hop)?;

    {
        let mut guard = IrqGuard::acquire(&NET);
        guard.state_mut().last_ping_reply = None;
    }

    let mut icmp = [0u8; 24];
    icmp[0] = 8; // Echo Request
    icmp[1] = 0;
    icmp[4..6].copy_from_slice(&ICMP_ECHO_ID.to_be_bytes());
    icmp[6..8].copy_from_slice(&seq.to_be_bytes());
    icmp[8..24].copy_from_slice(b"wmos-v4.1-probe!");
    let csum = rfc1071_checksum(&icmp);
    icmp[2..4].copy_from_slice(&csum.to_be_bytes());

    let start_tick = pit::ticks();
    send_ipv4_packet(dst_mac, target_ip, IP_PROTO_ICMP, &icmp)?;

    loop {
        poll();
        let matched = {
            let mut guard = IrqGuard::acquire(&NET);
            let st = guard.state_mut();
            if st.last_ping_reply == Some((target_ip, ICMP_ECHO_ID, seq)) {
                st.last_ping_reply = None;
                true
            } else {
                false
            }
        };
        if matched {
            let elapsed_ticks = pit::ticks().wrapping_sub(start_tick);
            let rtt_ms = (elapsed_ticks.max(1) * 1000) / pit::HZ;
            return Ok(rtt_ms);
        }
        if pit::ticks().wrapping_sub(start_tick) >= 150 {
            return Err("ICMP ping timed out");
        }
        wait_step();
    }
}

/// Sends a UDP datagram over IPv4 to `dst_ip:dst_port`.
pub fn send_udp(
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Result<(), &'static str> {
    if !is_up() {
        return Err("Network interface is down");
    }
    if payload.len() > 1472 {
        return Err("UDP payload exceeds IPv4 MTU");
    }

    let cfg = config();
    let next_hop = if same_subnet(dst_ip, cfg) {
        dst_ip
    } else {
        cfg.gateway
    };
    let dst_mac = resolve_mac(next_hop)?;

    let udp_len = 8 + payload.len();
    let mut udp_pkt = [0u8; 1480];
    udp_pkt[0..2].copy_from_slice(&src_port.to_be_bytes());
    udp_pkt[2..4].copy_from_slice(&dst_port.to_be_bytes());
    udp_pkt[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    udp_pkt[6..8].copy_from_slice(&[0, 0]);
    udp_pkt[8..udp_len].copy_from_slice(payload);

    send_ipv4_packet(dst_mac, dst_ip, IP_PROTO_UDP, &udp_pkt[..udp_len])
}

/// Parses a dotted-quad IPv4 address string such as `"10.0.2.2"`.
pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let trimmed = s.trim();
    let mut parts = trimmed.split('.');
    let mut out = [0u8; 4];

    for slot in &mut out {
        let seg = parts.next()?;
        if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = seg.parse::<u8>().ok()?;
    }

    if parts.next().is_some() {
        return None;
    }

    Some(out)
}
