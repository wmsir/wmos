use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::drivers::ahci;

const EFI_SYSTEM_PART_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

const MAX_DIR_DEPTH: usize = 4;
const MAX_FAT_FILES: usize = 256;
const MAX_CLUSTER_HOPS: usize = 8192;
const MAX_READ_BYTES: usize = 4 * 1024 * 1024;

/// Indexed FAT32 file location on disk.
#[derive(Clone)]
pub struct FatFileRecord {
    pub path: String,
    pub start_cluster: u32,
    pub size: usize,
}

/// Mounted read-only FAT32 partition metadata and file index.
pub struct Fat32Volume {
    pub sectors_per_cluster: u64,
    pub fat_start_lba: u64,
    pub data_start_lba: u64,
    pub files: Vec<FatFileRecord>,
}

fn find_fat32_partition_lba() -> Option<u64> {
    let mut sector = [0u8; 512];
    ahci::read_sectors(1, 1, &mut sector).ok()?;

    if &sector[0..8] == b"EFI PART" {
        let part_lba = u64::from_le_bytes([
            sector[72], sector[73], sector[74], sector[75], sector[76], sector[77], sector[78],
            sector[79],
        ]);
        let num_entries = u32::from_le_bytes([sector[80], sector[81], sector[82], sector[83]]);
        let entry_size = u32::from_le_bytes([sector[84], sector[85], sector[86], sector[87]]) as usize;

        if entry_size >= 128 && (512 % entry_size) == 0 {
            let per_sector = 512 / entry_size;
            let max_entries = (num_entries as usize).min(128);
            let mut cur_sec = u64::MAX;

            for idx in 0..max_entries {
                let sec = part_lba + (idx / per_sector) as u64;
                if sec != cur_sec {
                    ahci::read_sectors(sec, 1, &mut sector).ok()?;
                    cur_sec = sec;
                }
                let off = (idx % per_sector) * entry_size;
                if &sector[off..off + 16] == &EFI_SYSTEM_PART_GUID {
                    let first_lba = u64::from_le_bytes([
                        sector[off + 32],
                        sector[off + 33],
                        sector[off + 34],
                        sector[off + 35],
                        sector[off + 36],
                        sector[off + 37],
                        sector[off + 38],
                        sector[off + 39],
                    ]);
                    if first_lba > 0 {
                        return Some(first_lba);
                    }
                }
            }
        }
    }

    // Fallback: check LBA 0 superfloppy FAT32 BPB
    ahci::read_sectors(0, 1, &mut sector).ok()?;
    if sector[510] == 0x55 && sector[511] == 0xAA && &sector[82..87] == b"FAT32" {
        return Some(0);
    }

    None
}

fn next_cluster(fat_start_lba: u64, cluster: u32) -> Option<u32> {
    let fat_offset = (cluster as u64) * 4;
    let fat_sector = fat_start_lba + (fat_offset / 512);
    let ent_offset = (fat_offset % 512) as usize;

    let mut sec = [0u8; 512];
    ahci::read_sectors(fat_sector, 1, &mut sec).ok()?;
    let val = u32::from_le_bytes([
        sec[ent_offset],
        sec[ent_offset + 1],
        sec[ent_offset + 2],
        sec[ent_offset + 3],
    ]) & 0x0FFF_FFFF;

    if (2..0x0FFF_FFF8).contains(&val) {
        Some(val)
    } else {
        None
    }
}

fn read_chain(
    fat_start_lba: u64,
    data_start_lba: u64,
    sectors_per_cluster: u64,
    start_cluster: u32,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    if start_cluster < 2 {
        return None;
    }

    let cluster_bytes = (sectors_per_cluster as usize) * 512;
    let mut out = Vec::new();
    let mut cluster = Some(start_cluster);

    for _ in 0..MAX_CLUSTER_HOPS {
        let Some(c) = cluster else {
            break;
        };
        if out.len() >= max_bytes {
            break;
        }
        let lba = data_start_lba + ((c as u64 - 2) * sectors_per_cluster);
        let mut cbuf = vec![0u8; cluster_bytes];
        ahci::read_sectors(lba, sectors_per_cluster as u16, &mut cbuf).ok()?;

        let need = (max_bytes - out.len()).min(cluster_bytes);
        out.extend_from_slice(&cbuf[..need]);

        if out.len() >= max_bytes {
            break;
        }
        cluster = next_cluster(fat_start_lba, c);
    }

    Some(out)
}

fn decode_lfn(slots: &[[u16; 13]; 20], mask: u32) -> Option<String> {
    if (mask & 1) == 0 {
        return None;
    }

    let mut name = String::new();
    for slot in 0..20 {
        if (mask & (1 << slot)) == 0 {
            break;
        }
        for &code in &slots[slot] {
            if code == 0x0000 || code == 0xFFFF {
                return if name.is_empty() { None } else { Some(name) };
            }
            if code <= 0x7F {
                name.push((code as u8) as char);
            } else if let Some(ch) = char::from_u32(code as u32) {
                name.push(ch);
            } else {
                name.push('?');
            }
        }
    }

    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn decode_short_name(ent: &[u8]) -> String {
    let nt_flags = ent[12];
    let lower_base = (nt_flags & 0x08) != 0;
    let lower_ext = (nt_flags & 0x10) != 0;

    let mut out = String::new();
    for &raw in &ent[0..8] {
        if raw == b' ' {
            break;
        }
        let b = if raw == 0x05 { 0xE5 } else { raw };
        let ch = b as char;
        if lower_base {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }

    let mut has_ext = false;
    for &raw in &ent[8..11] {
        if raw != b' ' {
            has_ext = true;
            break;
        }
    }

    if has_ext {
        out.push('.');
        for &raw in &ent[8..11] {
            if raw == b' ' {
                break;
            }
            let ch = raw as char;
            if lower_ext {
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push(ch);
            }
        }
    }

    out
}

fn walk_dir(
    fat_start_lba: u64,
    data_start_lba: u64,
    sectors_per_cluster: u64,
    cluster: u32,
    prefix: &str,
    depth: usize,
    files: &mut Vec<FatFileRecord>,
) {
    if depth > MAX_DIR_DEPTH || files.len() >= MAX_FAT_FILES {
        return;
    }

    let Some(dir_data) = read_chain(
        fat_start_lba,
        data_start_lba,
        sectors_per_cluster,
        cluster,
        256 * 1024,
    ) else {
        return;
    };

    let mut lfn_slots = [[0u16; 13]; 20];
    let mut lfn_mask: u32 = 0;

    for ent in dir_data.chunks_exact(32) {
        let first = ent[0];
        if first == 0x00 {
            break;
        }
        if first == 0xE5 {
            lfn_mask = 0;
            continue;
        }

        let attr = ent[11];
        if attr == 0x0F {
            if (first & 0x40) != 0 {
                lfn_slots = [[0u16; 13]; 20];
                lfn_mask = 0;
            }
            let seq = (first & 0x1F) as usize;
            if (1..=20).contains(&seq) {
                let s = &mut lfn_slots[seq - 1];
                for i in 0..5 {
                    s[i] = u16::from_le_bytes([ent[1 + i * 2], ent[2 + i * 2]]);
                }
                for i in 0..6 {
                    s[5 + i] = u16::from_le_bytes([ent[14 + i * 2], ent[15 + i * 2]]);
                }
                for i in 0..2 {
                    s[11 + i] = u16::from_le_bytes([ent[28 + i * 2], ent[29 + i * 2]]);
                }
                lfn_mask |= 1 << (seq - 1);
            }
            continue;
        }

        let lfn_name = decode_lfn(&lfn_slots, lfn_mask);
        lfn_mask = 0;

        if (attr & 0x08) != 0 {
            continue;
        }

        let name = lfn_name.unwrap_or_else(|| decode_short_name(ent));
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }

        let hi_cluster = u16::from_le_bytes([ent[20], ent[21]]) as u32;
        let lo_cluster = u16::from_le_bytes([ent[26], ent[27]]) as u32;
        let start_cluster = (hi_cluster << 16) | lo_cluster;
        let size = u32::from_le_bytes([ent[28], ent[29], ent[30], ent[31]]) as usize;

        let full_path = if prefix.is_empty() {
            format!("disk/{}", name)
        } else {
            format!("{}/{}", prefix, name)
        };

        if (attr & 0x10) != 0 {
            if start_cluster >= 2 {
                walk_dir(
                    fat_start_lba,
                    data_start_lba,
                    sectors_per_cluster,
                    start_cluster,
                    &full_path,
                    depth + 1,
                    files,
                );
            }
        } else if files.len() < MAX_FAT_FILES {
            files.push(FatFileRecord {
                path: full_path,
                start_cluster,
                size,
            });
        }
    }
}

/// Scans the attached AHCI SATA disk for a GPT EFI/FAT32 partition and indexes all files.
pub fn scan_disk() -> Option<Fat32Volume> {
    if ahci::disk_info().is_none() {
        return None;
    }

    let part_lba = find_fat32_partition_lba()?;
    let mut bpb = [0u8; 512];
    ahci::read_sectors(part_lba, 1, &mut bpb).ok()?;

    let bytes_per_sector = u16::from_le_bytes([bpb[11], bpb[12]]) as u64;
    let sectors_per_cluster = bpb[13] as u64;
    let reserved_sectors = u16::from_le_bytes([bpb[14], bpb[15]]) as u64;
    let num_fats = bpb[16] as u64;
    let sectors_per_fat_32 = u32::from_le_bytes([bpb[36], bpb[37], bpb[38], bpb[39]]) as u64;
    let root_cluster = u32::from_le_bytes([bpb[44], bpb[45], bpb[46], bpb[47]]);

    if bytes_per_sector != 512
        || sectors_per_cluster == 0
        || num_fats == 0
        || sectors_per_fat_32 == 0
        || root_cluster < 2
    {
        return None;
    }

    let fat_start_lba = part_lba + reserved_sectors;
    let data_start_lba = fat_start_lba + num_fats * sectors_per_fat_32;

    let mut files = Vec::new();
    walk_dir(
        fat_start_lba,
        data_start_lba,
        sectors_per_cluster,
        root_cluster,
        "",
        0,
        &mut files,
    );
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Some(Fat32Volume {
        sectors_per_cluster,
        fat_start_lba,
        data_start_lba,
        files,
    })
}

/// Reads the contents of an indexed FAT32 file from disk via AHCI DMA.
pub fn read_file(vol: &Fat32Volume, rec: &FatFileRecord) -> Option<Vec<u8>> {
    if rec.size == 0 {
        return Some(Vec::new());
    }
    let limit = rec.size.min(MAX_READ_BYTES);
    read_chain(
        vol.fat_start_lba,
        vol.data_start_lba,
        vol.sectors_per_cluster,
        rec.start_cluster,
        limit,
    )
}
