use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{cpu, serial};
use crate::net;
use crate::net::tcp;
use crate::serial_println;

/// Transport protocol used by a registered smart-home device.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    WifiHttp,
    WifiMqtt,
    WifiUdp,
    UartMcu,
}

/// Smart-home / IoT device descriptor and cached state.
#[derive(Clone)]
pub struct SmartDevice {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: DeviceKind,
    pub ip: [u8; 4],
    pub port: u16,
    pub target: &'static str,
    pub on: bool,
    pub last_reply: String,
}

struct IotLock {
    locked: AtomicBool,
    devices: UnsafeCell<Option<Vec<SmartDevice>>>,
}

unsafe impl Sync for IotLock {}

static IOT: IotLock = IotLock {
    locked: AtomicBool::new(false),
    devices: UnsafeCell::new(None),
};

struct IrqGuard<'a> {
    lock: &'a IotLock,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a IotLock) -> Self {
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
    fn list_mut(&mut self) -> &mut Vec<SmartDevice> {
        let slot = unsafe { &mut *self.lock.devices.get() };
        slot.get_or_insert_with(Vec::new)
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

/// Seeds the 4 default smart-home devices (`lamp`, `ac`, `plug`, `mcu`).
pub fn init() {
    let mut guard = IrqGuard::acquire(&IOT);
    let devs = guard.list_mut();
    *devs = vec![
        SmartDevice {
            id: "lamp",
            name: "Desk Lamp (HTTP)",
            kind: DeviceKind::WifiHttp,
            ip: [10, 0, 2, 2],
            port: 18080,
            target: "/iot/lamp",
            on: false,
            last_reply: String::from("ready"),
        },
        SmartDevice {
            id: "ac",
            name: "Living AC (MQTT)",
            kind: DeviceKind::WifiMqtt,
            ip: [10, 0, 2, 2],
            port: 11883,
            target: "home/ac/power",
            on: false,
            last_reply: String::from("ready"),
        },
        SmartDevice {
            id: "plug",
            name: "Smart Plug (UDP)",
            kind: DeviceKind::WifiUdp,
            ip: [10, 0, 2, 2],
            port: 18888,
            target: "TOGGLE",
            on: false,
            last_reply: String::from("ready"),
        },
        SmartDevice {
            id: "mcu",
            name: "ESP32 MCU (COM2)",
            kind: DeviceKind::UartMcu,
            ip: [0, 0, 0, 0],
            port: 0x2F8,
            target: "LED",
            on: false,
            last_reply: String::from("ready"),
        },
    ];
}

/// Returns a cloned snapshot of all registered IoT devices.
pub fn list() -> Vec<SmartDevice> {
    let mut guard = IrqGuard::acquire(&IOT);
    guard.list_mut().clone()
}

/// Updates the target IPv4 address and TCP/UDP port of device `id`.
pub fn set_endpoint(id: &str, ip: [u8; 4], port: u16) -> Result<(), &'static str> {
    let mut guard = IrqGuard::acquire(&IOT);
    let devs = guard.list_mut();
    let Some(dev) = devs.iter_mut().find(|d| d.id.eq_ignore_ascii_case(id)) else {
        return Err("Unknown IoT device id");
    };
    dev.ip = ip;
    dev.port = port;
    Ok(())
}

/// Toggles device `id` over its real transport (HTTP, MQTT, UDP, or COM2 UART) and returns its reply.
pub fn toggle(id: &str) -> Result<String, &'static str> {
    let (kind, ip, port, target, next_on) = {
        let mut guard = IrqGuard::acquire(&IOT);
        let devs = guard.list_mut();
        let Some(dev) = devs.iter().find(|d| d.id.eq_ignore_ascii_case(id)) else {
            return Err("Unknown IoT device id");
        };
        (dev.kind, dev.ip, dev.port, dev.target, !dev.on)
    };

    let reply = match kind {
        DeviceKind::WifiHttp => {
            let path = format!("{}/{}", target, if next_on { "on" } else { "off" });
            let resp = tcp::http_get(ip, port, &path)?;
            let summary = resp
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("200 OK")
                .trim();
            String::from(summary)
        }
        DeviceKind::WifiMqtt => {
            let msg = if next_on { "on" } else { "off" };
            tcp::mqtt_publish(ip, port, target, msg)?;
            format!("MQTT {}={}", target, msg)
        }
        DeviceKind::WifiUdp => {
            let payload = if next_on {
                b"ON".as_slice()
            } else {
                b"OFF".as_slice()
            };
            let resp = net::udp_exchange(ip, port, payload, 100)?;
            let text = String::from_utf8_lossy(&resp);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                String::from(if next_on { "ON" } else { "OFF" })
            } else {
                String::from(trimmed)
            }
        }
        DeviceKind::UartMcu => {
            let cmd = if next_on { "LED=ON" } else { "LED=OFF" };
            serial::com2_exchange_line(cmd, 80).ok_or("COM2 MCU timeout")?
        }
    };

    {
        let mut guard = IrqGuard::acquire(&IOT);
        if let Some(dev) = guard
            .list_mut()
            .iter_mut()
            .find(|d| d.id.eq_ignore_ascii_case(id))
        {
            dev.on = next_on;
            dev.last_reply = reply.clone();
        }
    }

    serial_println!(
        "[wmos] iot toggle {} ok state={}",
        id,
        if next_on { "on" } else { "off" }
    );
    Ok(reply)
}

/// Sends a text command to the COM2 UART MCU and returns its trimmed reply line.
pub fn mcu_send(cmd: &str) -> Result<String, &'static str> {
    let reply = serial::com2_exchange_line(cmd, 80).ok_or("COM2 MCU timeout")?;

    {
        let mut guard = IrqGuard::acquire(&IOT);
        if let Some(dev) = guard.list_mut().iter_mut().find(|d| d.id == "mcu") {
            dev.last_reply = reply.clone();
            if reply.contains("LED=ON") {
                dev.on = true;
            } else if reply.contains("LED=OFF") {
                dev.on = false;
            }
        }
    }

    serial_println!("[wmos] mcu ok {}", reply);
    Ok(reply)
}
