use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};

use crate::arch::pit;
use crate::drivers::e1000;
use crate::net;

const IP_PROTO_TCP: u8 = 6;
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;
const MAX_HTTP_BYTES: usize = 4096;

static EPHEMERAL_TCP: AtomicU16 = AtomicU16::new(0);

struct TcpSegment {
    flags: u8,
    seq: u32,
    ack: u32,
    payload_start: usize,
    payload_end: usize,
}

struct TcpStream {
    dst_ip: [u8; 4],
    dst_mac: [u8; 6],
    src_port: u16,
    dst_port: u16,
    snd_nxt: u32,
    rcv_nxt: u32,
    rx_buf: Vec<u8>,
    peer_closed: bool,
}

impl TcpStream {
    fn send_segment(&self, flags: u8, seq: u32, ack: u32, payload: &[u8]) -> Result<(), &'static str> {
        let tcp_len = 20 + payload.len();
        if tcp_len > 1480 {
            return Err("TCP segment exceeds IPv4 MTU");
        }

        let src_ip = net::config().ip;
        let mut buf = [0u8; 1480];
        buf[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        buf[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        buf[4..8].copy_from_slice(&seq.to_be_bytes());
        buf[8..12].copy_from_slice(&ack.to_be_bytes());
        buf[12] = 0x50; // Data offset = 5 (20 bytes)
        buf[13] = flags;
        buf[14..16].copy_from_slice(&8192u16.to_be_bytes());
        buf[16..18].copy_from_slice(&[0, 0]);
        buf[18..20].copy_from_slice(&[0, 0]);
        if !payload.is_empty() {
            buf[20..tcp_len].copy_from_slice(payload);
        }

        let csum = net::pseudo_header_checksum(src_ip, self.dst_ip, IP_PROTO_TCP, &buf[..tcp_len]);
        buf[16..18].copy_from_slice(&csum.to_be_bytes());

        net::send_ipv4_packet(self.dst_mac, self.dst_ip, IP_PROTO_TCP, &buf[..tcp_len])
    }

    fn poll_rx_frame(&self, rx_buf: &mut [u8; 2048]) -> Option<TcpSegment> {
        for _ in 0..16 {
            let len = e1000::poll_recv(rx_buf)?;
            let frame = &rx_buf[..len];
            if frame.len() < 34 || u16::from_be_bytes([frame[12], frame[13]]) != 0x0800 {
                net::handle_non_tcp_frame(frame);
                continue;
            }

            let ip = &frame[14..];
            let ihl = ((ip[0] & 0x0F) as usize) * 4;
            if (ip[0] >> 4) != 4 || ihl < 20 || ip.len() < ihl {
                continue;
            }
            if ip[9] != IP_PROTO_TCP {
                net::handle_non_tcp_frame(frame);
                continue;
            }

            let total_len = (u16::from_be_bytes([ip[2], ip[3]]) as usize).min(ip.len());
            if total_len < ihl + 20 {
                continue;
            }
            let src_ip = [ip[12], ip[13], ip[14], ip[15]];
            let dst_ip = [ip[16], ip[17], ip[18], ip[19]];
            if src_ip != self.dst_ip || dst_ip != net::config().ip {
                continue;
            }

            let tcp_off = 14 + ihl;
            let tcp_end = 14 + total_len;
            let tcp = &frame[tcp_off..tcp_end];
            let sp = u16::from_be_bytes([tcp[0], tcp[1]]);
            let dp = u16::from_be_bytes([tcp[2], tcp[3]]);
            if sp != self.dst_port || dp != self.src_port {
                continue;
            }

            let data_off = ((tcp[12] >> 4) as usize) * 4;
            if data_off < 20 || tcp.len() < data_off {
                continue;
            }

            return Some(TcpSegment {
                flags: tcp[13],
                seq: u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]),
                ack: u32::from_be_bytes([tcp[8], tcp[9], tcp[10], tcp[11]]),
                payload_start: tcp_off + data_off,
                payload_end: tcp_end,
            });
        }

        None
    }

    fn ingest_segment(&mut self, seg: &TcpSegment, frame: &[u8]) {
        if (seg.flags & TCP_RST) != 0 {
            self.peer_closed = true;
            return;
        }

        let payload = &frame[seg.payload_start..seg.payload_end];
        let mut need_ack = false;

        if !payload.is_empty() && seg.seq == self.rcv_nxt {
            self.rx_buf.extend_from_slice(payload);
            self.rcv_nxt = self.rcv_nxt.wrapping_add(payload.len() as u32);
            need_ack = true;
        } else if !payload.is_empty() && (seg.seq < self.rcv_nxt) {
            need_ack = true;
        }

        if (seg.flags & TCP_FIN) != 0 {
            if seg.seq.wrapping_add(payload.len() as u32) == self.rcv_nxt {
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            }
            self.peer_closed = true;
            need_ack = true;
        }

        if need_ack {
            let _ = self.send_segment(TCP_ACK, self.snd_nxt, self.rcv_nxt, &[]);
        }
    }

    fn connect(dst_ip: [u8; 4], dst_port: u16) -> Result<Self, &'static str> {
        if !net::is_up() {
            return Err("Network interface is down");
        }

        let dst_mac = net::resolve_next_hop_mac(dst_ip)?;
        let src_port = 50000 + (EPHEMERAL_TCP.fetch_add(1, Ordering::Relaxed) % 10000);
        let isn = ((pit::ticks() as u32).wrapping_mul(2_654_435_761)) ^ 0x574D_5635;

        let mut stream = Self {
            dst_ip,
            dst_mac,
            src_port,
            dst_port,
            snd_nxt: isn,
            rcv_nxt: 0,
            rx_buf: Vec::new(),
            peer_closed: false,
        };

        stream.send_segment(TCP_SYN, isn, 0, &[])?;
        stream.snd_nxt = isn.wrapping_add(1);

        let start = pit::ticks();
        let mut retried = false;
        let mut rx_frame = [0u8; 2048];

        loop {
            while let Some(seg) = stream.poll_rx_frame(&mut rx_frame) {
                if (seg.flags & TCP_RST) != 0 {
                    return Err("TCP connection refused");
                }
                if (seg.flags & (TCP_SYN | TCP_ACK)) == (TCP_SYN | TCP_ACK)
                    && seg.ack == stream.snd_nxt
                {
                    stream.rcv_nxt = seg.seq.wrapping_add(1);
                    stream.send_segment(TCP_ACK, stream.snd_nxt, stream.rcv_nxt, &[])?;
                    return Ok(stream);
                }
            }

            let elapsed = pit::ticks().wrapping_sub(start);
            if elapsed >= 200 {
                return Err("TCP connect timed out");
            }
            if !retried && elapsed >= 100 {
                retried = true;
                let _ = stream.send_segment(TCP_SYN, isn, 0, &[]);
            }
            net::wait_step();
        }
    }

    fn send_all(&mut self, data: &[u8]) -> Result<(), &'static str> {
        let mut offset = 0usize;
        let mut rx_frame = [0u8; 2048];

        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(1024);
            let chunk = &data[offset..offset + chunk_len];
            let seq = self.snd_nxt;
            let expected_ack = seq.wrapping_add(chunk_len as u32);

            self.send_segment(TCP_PSH | TCP_ACK, seq, self.rcv_nxt, chunk)?;
            self.snd_nxt = expected_ack;

            let start = pit::ticks();
            loop {
                let mut acked = false;
                while let Some(seg) = self.poll_rx_frame(&mut rx_frame) {
                    if (seg.flags & TCP_ACK) != 0 && seg.ack.wrapping_sub(expected_ack) < 0x8000_0000 {
                        acked = true;
                    }
                    self.ingest_segment(&seg, &rx_frame);
                    if self.peer_closed && !acked {
                        return Err("TCP connection reset during send");
                    }
                }
                if acked {
                    break;
                }
                if pit::ticks().wrapping_sub(start) >= 150 {
                    return Err("TCP send ACK timed out");
                }
                net::wait_step();
            }

            offset += chunk_len;
        }

        Ok(())
    }

    fn recv_some(&mut self, min_bytes: usize, timeout_ticks: u64) -> Result<Vec<u8>, &'static str> {
        let start = pit::ticks();
        let mut rx_frame = [0u8; 2048];

        loop {
            while let Some(seg) = self.poll_rx_frame(&mut rx_frame) {
                self.ingest_segment(&seg, &rx_frame);
            }

            if self.rx_buf.len() >= min_bytes {
                let take = self.rx_buf.drain(..).collect();
                return Ok(take);
            }
            if self.peer_closed {
                if self.rx_buf.is_empty() {
                    return Err("TCP peer closed connection");
                }
                let take = self.rx_buf.drain(..).collect();
                return Ok(take);
            }
            if pit::ticks().wrapping_sub(start) >= timeout_ticks {
                return Err("TCP read timed out");
            }
            net::wait_step();
        }
    }

    fn recv_until_close(&mut self, timeout_ticks: u64) -> Vec<u8> {
        let start = pit::ticks();
        let mut last_rx = start;
        let mut rx_frame = [0u8; 2048];

        loop {
            let prev_len = self.rx_buf.len();
            while let Some(seg) = self.poll_rx_frame(&mut rx_frame) {
                self.ingest_segment(&seg, &rx_frame);
            }
            if self.rx_buf.len() > prev_len {
                last_rx = pit::ticks();
            }

            if self.peer_closed || self.rx_buf.len() >= MAX_HTTP_BYTES {
                break;
            }
            let now = pit::ticks();
            if now.wrapping_sub(start) >= timeout_ticks {
                break;
            }
            if !self.rx_buf.is_empty() && now.wrapping_sub(last_rx) >= 35 {
                break;
            }
            net::wait_step();
        }

        if self.rx_buf.len() > MAX_HTTP_BYTES {
            self.rx_buf.truncate(MAX_HTTP_BYTES);
        }
        self.rx_buf.drain(..).collect()
    }

    fn close(&mut self) {
        if !self.peer_closed {
            let _ = self.send_segment(TCP_FIN | TCP_ACK, self.snd_nxt, self.rcv_nxt, &[]);
            self.snd_nxt = self.snd_nxt.wrapping_add(1);
        }
    }
}

/// Performs a synchronous HTTP/1.0 GET request over TCP and returns the response text.
pub fn http_get(dst_ip: [u8; 4], port: u16, path: &str) -> Result<String, &'static str> {
    let clean_path = if path.starts_with('/') {
        String::from(path)
    } else {
        format!("/{}", path)
    };

    let req = format!(
        "GET {} HTTP/1.0\r\nHost: {}.{}.{}.{}\r\nConnection: close\r\n\r\n",
        clean_path, dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3]
    );

    let mut stream = TcpStream::connect(dst_ip, port)?;
    stream.send_all(req.as_bytes())?;
    let body = stream.recv_until_close(200);
    stream.close();

    if body.is_empty() {
        return Err("Empty HTTP response");
    }

    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn push_varint(buf: &mut Vec<u8>, mut len: usize) {
    loop {
        let mut byte = (len & 0x7F) as u8;
        len >>= 7;
        if len > 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if len == 0 {
            break;
        }
    }
}

/// Connects to an MQTT 3.1.1 broker over TCP, publishes a QoS-0 message, and disconnects cleanly.
pub fn mqtt_publish(
    broker_ip: [u8; 4],
    port: u16,
    topic: &str,
    payload: &str,
) -> Result<(), &'static str> {
    let mut stream = TcpStream::connect(broker_ip, port)?;

    let client_id = b"wmos-v5";
    let mut connect_body = Vec::with_capacity(12 + client_id.len());
    connect_body.extend_from_slice(&[0x00, 0x04, b'M', b'Q', b'T', b'T']);
    connect_body.push(4); // MQTT 3.1.1
    connect_body.push(0x02); // Clean Session
    connect_body.extend_from_slice(&60u16.to_be_bytes());
    connect_body.extend_from_slice(&(client_id.len() as u16).to_be_bytes());
    connect_body.extend_from_slice(client_id);

    let mut connect_pkt = Vec::with_capacity(2 + connect_body.len());
    connect_pkt.push(0x10);
    push_varint(&mut connect_pkt, connect_body.len());
    connect_pkt.extend_from_slice(&connect_body);

    stream.send_all(&connect_pkt)?;
    let connack = stream.recv_some(4, 150)?;
    if connack.len() < 4 || connack[0] != 0x20 || connack[3] != 0x00 {
        stream.close();
        return Err("MQTT broker rejected CONNECT");
    }

    let topic_bytes = topic.as_bytes();
    let payload_bytes = payload.as_bytes();
    let rem_len = 2 + topic_bytes.len() + payload_bytes.len();

    let mut pub_pkt = Vec::with_capacity(5 + rem_len);
    pub_pkt.push(0x30); // PUBLISH QoS 0
    push_varint(&mut pub_pkt, rem_len);
    pub_pkt.extend_from_slice(&(topic_bytes.len() as u16).to_be_bytes());
    pub_pkt.extend_from_slice(topic_bytes);
    pub_pkt.extend_from_slice(payload_bytes);

    stream.send_all(&pub_pkt)?;
    let _ = stream.send_all(&[0xE0, 0x00]);
    stream.close();

    Ok(())
}
