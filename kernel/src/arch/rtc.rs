use crate::arch::io::{inb, outb};

const CMOS_ADDR: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

/// Calendar date and wall-clock time read from the PC CMOS RTC.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RtcDateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[inline]
fn cmos_read(reg: u8) -> u8 {
    unsafe {
        outb(CMOS_ADDR, 0x80 | reg);
        inb(CMOS_DATA)
    }
}

fn wait_uip_clear() {
    for _ in 0..100_000 {
        if (cmos_read(0x0A) & 0x80) == 0 {
            break;
        }
        core::hint::spin_loop();
    }
}

#[inline]
const fn bcd_to_bin(val: u8) -> u8 {
    (val & 0x0F) + ((val >> 4) * 10)
}

fn read_raw_regs() -> [u8; 8] {
    wait_uip_clear();
    [
        cmos_read(0x00), // second
        cmos_read(0x02), // minute
        cmos_read(0x04), // hour
        cmos_read(0x07), // day
        cmos_read(0x08), // month
        cmos_read(0x09), // year (2-digit)
        cmos_read(0x32), // century
        cmos_read(0x0B), // status register B
    ]
}

/// Reads a consistent snapshot of the CMOS Real-Time Clock.
pub fn read() -> RtcDateTime {
    let mut prev = read_raw_regs();
    for _ in 0..16 {
        let curr = read_raw_regs();
        if curr == prev {
            break;
        }
        prev = curr;
    }

    unsafe {
        outb(CMOS_ADDR, 0x0D);
    }

    let mut second = prev[0];
    let mut minute = prev[1];
    let mut hour = prev[2];
    let mut day = prev[3];
    let mut month = prev[4];
    let mut yr2 = prev[5];
    let mut century = prev[6];
    let reg_b = prev[7];

    if (reg_b & 0x04) == 0 {
        second = bcd_to_bin(second);
        minute = bcd_to_bin(minute);
        hour = bcd_to_bin(hour & 0x7F) | (hour & 0x80);
        day = bcd_to_bin(day);
        month = bcd_to_bin(month);
        yr2 = bcd_to_bin(yr2);
        century = bcd_to_bin(century);
    }

    if (reg_b & 0x02) == 0 {
        if (hour & 0x80) != 0 {
            hour = ((hour & 0x7F) % 12) + 12;
        } else if hour == 12 {
            hour = 0;
        }
    }

    if century == 0 || century > 99 {
        century = 20;
    }

    let year = (century as u16) * 100 + (yr2 as u16);

    RtcDateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    }
}
