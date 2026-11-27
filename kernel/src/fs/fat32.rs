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
const FAT_EOC: u32 = 0x0FFF_FFFF;

/// Indexed FAT32 file location on disk.
#[derive(Clone)]
pub struct FatFileRecord {
    pub path: String,
    pub start_cluster: u32,
    pub size: usize,
}

/// Mounted FAT32 partition metadata and file index.
pub struct Fat32Volume {
    pub sectors_per_cluster: u64,
    pub fat_start_lba: u64,
    pub data_start_lba: u64,
    pub num_fats: u64,
    pub sectors_per_fat: u64,
    pub root_cluster: u32,
    pub total_clusters: u32,
    pub files: Vec<FatFileRecord>,
}

#[inline]
fn cluster_to_lba(data_start_lba: u64, sectors_per_cluster: u64, cluster: u32) -> u64 {
    data_start_lba + ((cluster as u64 - 2) * sectors_per_cluster)
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

fn set_fat_entry(vol: &Fat32Volume, cluster: u32, val: u32) -> Result<(), &'static str> {
    if cluster < 2 || cluster >= vol.total_clusters.saturating_add(2) {
        return Err("Cluster index out of range");
    }

    let fat_offset = (cluster as u64) * 4;
    let rel_sector = fat_offset / 512;
    let ent_off = (fat_offset % 512) as usize;
    let mut sec = [0u8; 512];

    for fat_idx in 0..vol.num_fats {
        let sec_lba = vol.fat_start_lba + fat_idx * vol.sectors_per_fat + rel_sector;
        ahci::read_sectors(sec_lba, 1, &mut sec)?;
        let existing = u32::from_le_bytes([
            sec[ent_off],
            sec[ent_off + 1],
            sec[ent_off + 2],
            sec[ent_off + 3],
        ]);
        let updated = (existing & 0xF000_0000) | (val & 0x0FFF_FFFF);
        let bytes = updated.to_le_bytes();
        sec[ent_off..ent_off + 4].copy_from_slice(&bytes);
        ahci::write_sectors(sec_lba, 1, &sec)?;
    }

    Ok(())
}

fn free_chain(vol: &Fat32Volume, start_cluster: u32) -> Result<(), &'static str> {
    let mut cluster = start_cluster;
    for _ in 0..MAX_CLUSTER_HOPS {
        if cluster < 2 || cluster >= vol.total_clusters.saturating_add(2) {
            break;
        }
        let next = next_cluster(vol.fat_start_lba, cluster);
        set_fat_entry(vol, cluster, 0)?;
        match next {
            Some(nc) => cluster = nc,
            None => break,
        }
    }
    Ok(())
}

fn alloc_chain(vol: &Fat32Volume, num_clusters: usize) -> Result<u32, &'static str> {
    if num_clusters == 0 {
        return Ok(0);
    }

    let max_cluster = vol.total_clusters.saturating_add(2);
    let mut found = Vec::with_capacity(num_clusters);
    let mut sec = [0u8; 512];

    let mut c = 2u32;
    let mut loaded_sec = u64::MAX;

    while c < max_cluster && found.len() < num_clusters {
        let fat_offset = (c as u64) * 4;
        let rel_sec = fat_offset / 512;
        let ent_off = (fat_offset % 512) as usize;

        if rel_sec != loaded_sec {
            ahci::read_sectors(vol.fat_start_lba + rel_sec, 1, &mut sec)?;
            loaded_sec = rel_sec;
        }

        let val = u32::from_le_bytes([
            sec[ent_off],
            sec[ent_off + 1],
            sec[ent_off + 2],
            sec[ent_off + 3],
        ]) & 0x0FFF_FFFF;

        if val == 0 {
            found.push(c);
        }
        c += 1;
    }

    if found.len() < num_clusters {
        return Err("FAT32 disk full");
    }

    for i in 0..found.len() {
        let next_val = if i + 1 < found.len() {
            found[i + 1]
        } else {
            FAT_EOC
        };
        if let Err(e) = set_fat_entry(vol, found[i], next_val) {
            for &written in &found[..i] {
                let _ = set_fat_entry(vol, written, 0);
            }
            return Err(e);
        }
    }

    Ok(found[0])
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
        let lba = cluster_to_lba(data_start_lba, sectors_per_cluster, c);
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

fn is_valid_83_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn encode_short_name(name: &str) -> Option<([u8; 11], u8)> {
    if name.is_empty() || name.contains('/') || !name.is_ascii() {
        return None;
    }

    let (base, ext) = match name.split_once('.') {
        Some((b, e)) => {
            if e.is_empty() || e.contains('.') {
                return None;
            }
            (b, e)
        }
        None => (name, ""),
    };

    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }

    let mut base_lower = false;
    let mut base_upper = false;
    for b in base.bytes() {
        if !is_valid_83_char(b) {
            return None;
        }
        if b.is_ascii_lowercase() {
            base_lower = true;
        } else if b.is_ascii_uppercase() {
            base_upper = true;
        }
    }
    if base_lower && base_upper {
        return None;
    }

    let mut ext_lower = false;
    let mut ext_upper = false;
    for b in ext.bytes() {
        if !is_valid_83_char(b) {
            return None;
        }
        if b.is_ascii_lowercase() {
            ext_lower = true;
        } else if b.is_ascii_uppercase() {
            ext_upper = true;
        }
    }
    if ext_lower && ext_upper {
        return None;
    }

    let mut short = [b' '; 11];
    for (i, b) in base.bytes().enumerate() {
        short[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().enumerate() {
        short[8 + i] = b.to_ascii_uppercase();
    }

    let nt_flags = (if base_lower { 0x08 } else { 0 }) | (if ext_lower { 0x10 } else { 0 });
    Some((short, nt_flags))
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

struct RootSlotMatch {
    sector_lba: u64,
    in_sec_off: usize,
    start_cluster: u32,
    is_dir: bool,
}

fn scan_root_dir(
    vol: &Fat32Volume,
    short_name: &[u8; 11],
    name: &str,
) -> Result<(Option<RootSlotMatch>, Option<(u64, usize)>, u32), &'static str> {
    let cluster_bytes = (vol.sectors_per_cluster as usize) * 512;
    let mut cbuf = vec![0u8; cluster_bytes];

    let mut cluster = Some(vol.root_cluster);
    let mut last_cluster = vol.root_cluster;
    let mut matched: Option<RootSlotMatch> = None;
    let mut free_slot: Option<(u64, usize)> = None;

    for _ in 0..MAX_CLUSTER_HOPS {
        let Some(c) = cluster else {
            break;
        };
        last_cluster = c;
        let base_lba = cluster_to_lba(vol.data_start_lba, vol.sectors_per_cluster, c);
        ahci::read_sectors(base_lba, vol.sectors_per_cluster as u16, &mut cbuf)?;

        let mut lfn_slots = [[0u16; 13]; 20];
        let mut lfn_mask: u32 = 0;

        for (idx, ent) in cbuf.chunks_exact(32).enumerate() {
            let byte_off = idx * 32;
            let sector_lba = base_lba + (byte_off / 512) as u64;
            let in_sec_off = byte_off % 512;
            let first = ent[0];

            if first == 0x00 {
                if free_slot.is_none() {
                    free_slot = Some((sector_lba, in_sec_off));
                }
                return Ok((matched, free_slot, last_cluster));
            }
            if first == 0xE5 {
                lfn_mask = 0;
                if free_slot.is_none() {
                    free_slot = Some((sector_lba, in_sec_off));
                }
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

            let is_match = &ent[0..11] == short_name
                || lfn_name
                    .as_deref()
                    .map_or(false, |ln| ln.eq_ignore_ascii_case(name));

            if is_match {
                let hi = u16::from_le_bytes([ent[20], ent[21]]) as u32;
                let lo = u16::from_le_bytes([ent[26], ent[27]]) as u32;
                matched = Some(RootSlotMatch {
                    sector_lba,
                    in_sec_off,
                    start_cluster: (hi << 16) | lo,
                    is_dir: (attr & 0x10) != 0,
                });
                return Ok((matched, free_slot, last_cluster));
            }
        }

        cluster = next_cluster(vol.fat_start_lba, c);
    }

    Ok((matched, free_slot, last_cluster))
}

fn refresh_index(vol: &mut Fat32Volume) {
    vol.files.clear();
    walk_dir(
        vol.fat_start_lba,
        vol.data_start_lba,
        vol.sectors_per_cluster,
        vol.root_cluster,
        "",
        0,
        &mut vol.files,
    );
    vol.files.sort_by(|a, b| a.path.cmp(&b.path));
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
    let total_sectors_16 = u16::from_le_bytes([bpb[19], bpb[20]]) as u64;
    let total_sectors_32 = u32::from_le_bytes([bpb[32], bpb[33], bpb[34], bpb[35]]) as u64;
    let sectors_per_fat = u32::from_le_bytes([bpb[36], bpb[37], bpb[38], bpb[39]]) as u64;
    let root_cluster = u32::from_le_bytes([bpb[44], bpb[45], bpb[46], bpb[47]]);

    if bytes_per_sector != 512
        || sectors_per_cluster == 0
        || num_fats == 0
        || sectors_per_fat == 0
        || root_cluster < 2
    {
        return None;
    }

    let fat_start_lba = part_lba + reserved_sectors;
    let data_start_lba = fat_start_lba + num_fats * sectors_per_fat;

    let total_sectors = if total_sectors_32 != 0 {
        total_sectors_32
    } else {
        total_sectors_16
    };
    let data_offset = data_start_lba.saturating_sub(part_lba);
    let by_bpb = total_sectors.saturating_sub(data_offset) / sectors_per_cluster;
    let by_fat = ((sectors_per_fat * 512) / 4).saturating_sub(2);
    let total_clusters = by_bpb.min(by_fat).min(u32::MAX as u64) as u32;

    if total_clusters < 1 {
        return None;
    }

    let mut vol = Fat32Volume {
        sectors_per_cluster,
        fat_start_lba,
        data_start_lba,
        num_fats,
        sectors_per_fat,
        root_cluster,
        total_clusters,
        files: Vec::new(),
    };
    refresh_index(&mut vol);

    Some(vol)
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

/// Creates or overwrites a root-directory 8.3 file on the FAT32 disk.
pub fn write_root_file(
    vol: &mut Fat32Volume,
    name: &str,
    data: &[u8],
) -> Result<usize, &'static str> {
    let (short_name, nt_flags) =
        encode_short_name(name).ok_or("Invalid 8.3 filename (use root /disk/<name.ext>)")?;
    if data.len() > MAX_READ_BYTES {
        return Err("File exceeds maximum write size");
    }

    let (matched, free_slot, last_dir_cluster) = scan_root_dir(vol, &short_name, name)?;
    let (target_sec_lba, target_in_sec_off, old_start_cluster) = match matched {
        Some(m) => {
            if m.is_dir {
                return Err("Cannot overwrite directory");
            }
            (m.sector_lba, m.in_sec_off, m.start_cluster)
        }
        None => match free_slot {
            Some((sec, off)) => (sec, off, 0),
            None => {
                let new_dir_cluster = alloc_chain(vol, 1)?;
                let cluster_bytes = (vol.sectors_per_cluster as usize) * 512;
                let zero_cluster = vec![0u8; cluster_bytes];
                let new_lba =
                    cluster_to_lba(vol.data_start_lba, vol.sectors_per_cluster, new_dir_cluster);
                ahci::write_sectors(new_lba, vol.sectors_per_cluster as u16, &zero_cluster)?;
                set_fat_entry(vol, last_dir_cluster, new_dir_cluster)?;
                (new_lba, 0, 0)
            }
        },
    };

    let cluster_bytes = (vol.sectors_per_cluster as usize) * 512;
    let first_cluster = if !data.is_empty() {
        let clusters_needed = data.len().div_ceil(cluster_bytes);
        let head = alloc_chain(vol, clusters_needed)?;

        let mut cur = Some(head);
        let mut offset = 0usize;
        let mut cbuf = vec![0u8; cluster_bytes];

        while offset < data.len() {
            let Some(c) = cur else {
                let _ = free_chain(vol, head);
                return Err("Corrupt allocated cluster chain");
            };
            cbuf.fill(0);
            let take = (data.len() - offset).min(cluster_bytes);
            cbuf[..take].copy_from_slice(&data[offset..offset + take]);

            let lba = cluster_to_lba(vol.data_start_lba, vol.sectors_per_cluster, c);
            if let Err(e) = ahci::write_sectors(lba, vol.sectors_per_cluster as u16, &cbuf) {
                let _ = free_chain(vol, head);
                return Err(e);
            }

            offset += take;
            if offset < data.len() {
                cur = next_cluster(vol.fat_start_lba, c);
            }
        }
        head
    } else {
        0
    };

    let mut sec = [0u8; 512];
    ahci::read_sectors(target_sec_lba, 1, &mut sec)?;
    let ent = &mut sec[target_in_sec_off..target_in_sec_off + 32];
    ent.fill(0);
    ent[0..11].copy_from_slice(&short_name);
    ent[11] = 0x20;
    ent[12] = nt_flags;
    let hi = ((first_cluster >> 16) as u16).to_le_bytes();
    let lo = ((first_cluster & 0xFFFF) as u16).to_le_bytes();
    let sz = (data.len() as u32).to_le_bytes();
    ent[20..22].copy_from_slice(&hi);
    ent[26..28].copy_from_slice(&lo);
    ent[28..32].copy_from_slice(&sz);

    if let Err(e) = ahci::write_sectors(target_sec_lba, 1, &sec) {
        if first_cluster >= 2 {
            let _ = free_chain(vol, first_cluster);
        }
        return Err(e);
    }

    if old_start_cluster >= 2 {
        let _ = free_chain(vol, old_start_cluster);
    }

    refresh_index(vol);
    Ok(data.len())
}

/// Deletes a root-directory 8.3 file from the FAT32 disk and frees its cluster chain.
pub fn delete_root_file(vol: &mut Fat32Volume, name: &str) -> Result<(), &'static str> {
    let (short_name, _) =
        encode_short_name(name).ok_or("Invalid 8.3 filename (use root /disk/<name.ext>)")?;

    let (matched, _, _) = scan_root_dir(vol, &short_name, name)?;
    let m = matched.ok_or("File not found")?;
    if m.is_dir {
        return Err("Cannot remove directory");
    }

    let mut sec = [0u8; 512];
    ahci::read_sectors(m.sector_lba, 1, &mut sec)?;
    sec[m.in_sec_off] = 0xE5;
    ahci::write_sectors(m.sector_lba, 1, &sec)?;

    if m.start_cluster >= 2 {
        free_chain(vol, m.start_cluster)?;
    }

    refresh_index(vol);
    Ok(())
}
