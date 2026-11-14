//! Section renaming for ELF objects (ADR 0101, P6).
//!
//! cranelift-object can place a *function* in its own section but not name
//! it, and its `object` writer keeps section names private. The name only lives
//! in `.shstrtab`, so renaming is a patch of the finished file: append the
//! new names to a copy of the string table at the end of the file and point
//! the section headers at them. Nothing else moves, so every offset, symbol
//! and relocation stays valid.
use crate::{Error, Result};
use std::collections::HashMap;

fn bad() -> Error {
    Error::bug("unexpected ELF layout while naming sections")
}

fn u16_at(b: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(b.get(at..at + 2).ok_or_else(bad)?.try_into().unwrap()))
}
fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(b.get(at..at + 4).ok_or_else(bad)?.try_into().unwrap()))
}
fn u64_at(b: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(b.get(at..at + 8).ok_or_else(bad)?.try_into().unwrap()))
}

fn c_string(table: &[u8], at: usize) -> Result<&[u8]> {
    let rest = table.get(at..).ok_or_else(bad)?;
    Ok(&rest[..rest.iter().position(|c| *c == 0).ok_or_else(bad)?])
}

/// Rename the section holding each named symbol: `symbol name -> section name`.
pub(crate) fn rename_sections(mut file: Vec<u8>, by_symbol: &HashMap<Vec<u8>, Vec<u8>>) -> Result<Vec<u8>> {
    if file.get(..6) != Some(&[0x7f, b'E', b'L', b'F', 2, 1]) {
        return Err(bad());
    }
    let shoff = u64_at(&file, 0x28)? as usize;
    let entry = u16_at(&file, 0x3a)? as usize;
    let count = u16_at(&file, 0x3c)? as usize;
    let shstrndx = u16_at(&file, 0x3e)? as usize;
    let header = |i: usize| shoff + i * entry;
    let symtab = (0..count).find(|i| u32_at(&file, header(*i) + 4).is_ok_and(|t| t == 2)).ok_or_else(bad)?;
    let strtab = u32_at(&file, header(symtab) + 40)? as usize;
    let (sym_off, sym_size) = (u64_at(&file, header(symtab) + 24)? as usize, u64_at(&file, header(symtab) + 32)? as usize);
    let (str_off, str_size) = (u64_at(&file, header(strtab) + 24)? as usize, u64_at(&file, header(strtab) + 32)? as usize);
    let names = file.get(str_off..str_off + str_size).ok_or_else(bad)?.to_vec();
    let (old_off, old_size) = (u64_at(&file, header(shstrndx) + 24)? as usize, u64_at(&file, header(shstrndx) + 32)? as usize);
    let mut table = file.get(old_off..old_off + old_size).ok_or_else(bad)?.to_vec();
    let mut renamed: HashMap<usize, usize> = HashMap::new();
    let mut spelled: HashMap<usize, Vec<u8>> = HashMap::new();
    for n in 0..sym_size / 24 {
        let at = sym_off + n * 24;
        let section = u16_at(&file, at + 6)? as usize;
        if section == 0 || section >= 0xff00 || renamed.contains_key(&section) {
            continue;
        }
        if let Some(name) = by_symbol.get(c_string(&names, u32_at(&file, at)? as usize)?) {
            renamed.insert(section, table.len());
            spelled.insert(section, name.clone());
            table.extend_from_slice(name);
            table.push(0);
        }
    }
    // Relocation sections are named after their target (`.rela.text.boot`).
    for i in 1..count {
        let (kind, target) = (u32_at(&file, header(i) + 4)?, u32_at(&file, header(i) + 44)? as usize);
        if let (4 | 9, Some(name)) = (kind, spelled.get(&target)) {
            let prefix: &[u8] = if kind == 4 { b".rela" } else { b".rel" };
            renamed.insert(i, table.len());
            table.extend_from_slice(prefix);
            table.extend_from_slice(name);
            table.push(0);
        }
    }
    for (section, offset) in renamed {
        file[header(section)..header(section) + 4].copy_from_slice(&(offset as u32).to_le_bytes());
    }
    let new_off = file.len();
    file.extend_from_slice(&table);
    file[header(shstrndx) + 24..header(shstrndx) + 32].copy_from_slice(&(new_off as u64).to_le_bytes());
    file[header(shstrndx) + 32..header(shstrndx) + 40].copy_from_slice(&(table.len() as u64).to_le_bytes());
    Ok(file)
}
