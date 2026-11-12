//! Reading and writing the buffer as raw binary or Intel HEX.

use crate::lang::{tr, trf};
use std::path::Path;

pub fn is_hex_path(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("hex") | Some("ihx") | Some("h86"))
}

pub fn load(path: &Path) -> Result<Vec<u8>, String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    if is_hex_path(path) {
        parse_ihex(&String::from_utf8_lossy(&raw))
    } else {
        Ok(raw)
    }
}

pub fn save(path: &Path, data: &[u8]) -> Result<(), String> {
    let bytes = if is_hex_path(path) { to_ihex(data).into_bytes() } else { data.to_vec() };
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn hex_byte(s: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(s.get(i..i + 2)?, 16).ok()
}

/// Intel HEX -> flat image starting at address 0 (gaps are filled with 0xFF).
pub fn parse_ihex(text: &str) -> Result<Vec<u8>, String> {
    let mut base: u64 = 0;
    let mut chunks: Vec<(u64, Vec<u8>)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let body = line.strip_prefix(':').ok_or(trf!("第 {} 行不是 Intel HEX 记录", "Line {} is not an Intel HEX record", n + 1))?;
        let len = hex_byte(body, 0).ok_or(tr("记录格式错误", "Malformed record"))? as usize;
        let addr = ((hex_byte(body, 2).ok_or(tr("记录格式错误", "Malformed record"))? as usize) << 8) | hex_byte(body, 4).ok_or(tr("记录格式错误", "Malformed record"))? as usize;
        let typ = hex_byte(body, 6).ok_or(tr("记录格式错误", "Malformed record"))?;
        let mut data = Vec::with_capacity(len);
        for i in 0..len {
            data.push(hex_byte(body, 8 + i * 2).ok_or(trf!("第 {} 行数据不完整", "Line {} has incomplete data", n + 1))?);
        }
        let sum: u32 = (0..len + 5).filter_map(|i| hex_byte(body, i * 2)).map(|b| b as u32).sum();
        if sum & 0xFF != 0 {
            return Err(trf!("第 {} 行校验和错误", "Line {} has a bad checksum", n + 1));
        }
        match typ {
            0x00 => chunks.push((base + addr as u64, data)),
            0x01 => break,
            0x02 | 0x04 => {
                if data.len() != 2 {
                    return Err(trf!("第 {} 行的扩展地址记录长度不对", "Line {} has an extended address record of the wrong length", n + 1));
                }
                let v = ((data[0] as u64) << 8) | data[1] as u64;
                base = if typ == 0x02 { v << 4 } else { v << 16 };
            }
            _ => {}
        }
    }
    // u64 arithmetic: on this 32-bit program a usize sum of base + address + length could overflow
    const MAX: u64 = 256 * 1024 * 1024;
    let end = chunks.iter().map(|(a, d)| a + d.len() as u64).max().unwrap_or(0);
    if end == 0 {
        return Err(tr("文件中没有数据", "The file contains no data").into());
    }
    if end > MAX {
        return Err(tr("地址范围过大", "Address range too large").into());
    }
    let mut img = vec![0xFFu8; end as usize];
    for (a, d) in chunks {
        let a = a as usize;
        img[a..a + d.len()].copy_from_slice(&d);
    }
    Ok(img)
}

pub fn to_ihex(data: &[u8]) -> String {
    fn rec(out: &mut String, typ: u8, addr: u16, d: &[u8]) {
        let mut sum = d.len() as u32 + (addr >> 8) as u32 + (addr & 0xFF) as u32 + typ as u32;
        out.push_str(&format!(":{:02X}{:04X}{:02X}", d.len(), addr, typ));
        for b in d {
            out.push_str(&format!("{b:02X}"));
            sum += *b as u32;
        }
        out.push_str(&format!("{:02X}\n", (0x100 - (sum & 0xFF)) & 0xFF));
    }
    let mut out = String::new();
    let mut high = u32::MAX;
    for (i, chunk) in data.chunks(16).enumerate() {
        let addr = i * 16;
        if (addr >> 16) as u32 != high {
            high = (addr >> 16) as u32;
            rec(&mut out, 0x04, 0, &[(high >> 8) as u8, high as u8]);
        }
        rec(&mut out, 0x00, (addr & 0xFFFF) as u16, chunk);
    }
    rec(&mut out, 0x01, 0, &[]);
    out
}
