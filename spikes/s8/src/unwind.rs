//! Windows unwind tables for guest code, translated from `.eh_frame`
//! (x64 only).
//!
//! Windows unwinds a thread (exception dispatch, `RtlVirtualUnwind`,
//! debuggers) with `RUNTIME_FUNCTION` and `UNWIND_INFO` records. Guest ELF
//! code only has DWARF call frame information, so without a translation a
//! guest frame looks like a leaf function to Windows. This module replays
//! each FDE's CFA program over the prologue and emits the matching unwind
//! codes:
//!
//! - CFA offset grows by 8 and a register is saved at the new CFA - offset:
//!   `UWOP_PUSH_NONVOL`;
//! - CFA offset grows without a save: `UWOP_ALLOC_SMALL` / `UWOP_ALLOC_LARGE`;
//! - CFA register becomes `rbp` (after `push rbp; mov rbp, rsp`):
//!   `UWOP_SET_FPREG`.
//!
//! Functions whose CFI does not fit this shape (CFA on another register,
//! saves at other offsets, prologues longer than 255 bytes) get no entry
//! and are reported. Windows applies the codes after `UWOP_SET_FPREG`
//! relative to `rsp`, which is right unless the function changes `rsp`
//! dynamically (`alloca`).

use crate::loader::Object;

#[cfg(not(target_arch = "x86_64"))]
pub fn register(_o: &Object) -> Result<usize, String> {
    Err("unwind translation is implemented for x64 only".into())
}

#[cfg(target_arch = "x86_64")]
pub use x64::register;

#[cfg(target_arch = "x86_64")]
mod x64 {
    use super::*;
    use crate::elf::PT_GNU_EH_FRAME;
    use windows::Win32::System::Diagnostics::Debug::{
        IMAGE_RUNTIME_FUNCTION_ENTRY, RtlAddFunctionTable,
    };

    struct Reader<'a> {
        d: &'a [u8],
        p: usize,
        base: u64,
    }

    impl Reader<'_> {
        fn u8(&mut self) -> Option<u8> {
            let v = *self.d.get(self.p)?;
            self.p += 1;
            Some(v)
        }
        fn u16(&mut self) -> Option<u16> {
            Some(self.u8()? as u16 | (self.u8()? as u16) << 8)
        }
        fn u32(&mut self) -> Option<u32> {
            Some(self.u16()? as u32 | (self.u16()? as u32) << 16)
        }
        fn u64(&mut self) -> Option<u64> {
            Some(self.u32()? as u64 | (self.u32()? as u64) << 32)
        }
        fn uleb(&mut self) -> Option<u64> {
            let (mut v, mut s) = (0u64, 0);
            loop {
                let b = self.u8()?;
                v |= ((b & 0x7f) as u64) << s;
                s += 7;
                if b & 0x80 == 0 {
                    return Some(v);
                }
            }
        }
        fn sleb(&mut self) -> Option<i64> {
            let (mut v, mut s) = (0i64, 0);
            loop {
                let b = self.u8()?;
                v |= ((b & 0x7f) as i64) << s;
                s += 7;
                if b & 0x80 == 0 {
                    if s < 64 && b & 0x40 != 0 {
                        v |= -1 << s;
                    }
                    return Some(v);
                }
            }
        }
        /// A pointer in DW_EH_PE encoding `enc`; `addr` of the field itself
        /// for pc-relative values.
        fn ptr(&mut self, enc: u8) -> Option<u64> {
            let here = self.base + self.p as u64;
            let v = match enc & 0x0f {
                0x00 => self.u64()?,
                0x01 => self.uleb()?,
                0x03 => self.u32()? as u64,
                0x0b => self.u32()? as i32 as i64 as u64,
                0x09 => self.sleb()? as u64,
                0x04 | 0x0c => self.u64()?,
                0x02 => self.u16()? as u64,
                0x0a => self.u16()? as i16 as i64 as u64,
                _ => return None,
            };
            Some(match enc & 0x70 {
                0x00 => v,
                0x10 => here.wrapping_add(v),
                _ => return None,
            })
        }
    }

    struct Cie {
        code_align: u64,
        data_align: i64,
        fde_enc: u8,
        insns: Vec<u8>,
    }

    fn parse_cie(mem: &[u8], base: u64, at: usize) -> Option<Cie> {
        let mut r = Reader {
            d: mem,
            p: at,
            base,
        };
        let len = r.u32()? as usize;
        let end = r.p + len;
        let _id = r.u32()?;
        let _ver = r.u8()?;
        let mut aug = vec![];
        loop {
            let c = r.u8()?;
            if c == 0 {
                break;
            }
            aug.push(c);
        }
        let code_align = r.uleb()?;
        let data_align = r.sleb()?;
        let _ra = r.uleb()?;
        let mut fde_enc = 0;
        if aug.first() == Some(&b'z') {
            let alen = r.uleb()? as usize;
            let aend = r.p + alen;
            for &c in &aug[1..] {
                match c {
                    b'R' => fde_enc = r.u8()?,
                    b'P' => {
                        let e = r.u8()?;
                        r.ptr(e)?;
                    }
                    b'L' => {
                        r.u8()?;
                    }
                    _ => {}
                }
            }
            r.p = aend;
        }
        Some(Cie {
            code_align,
            data_align,
            fde_enc,
            insns: mem.get(r.p..end)?.to_vec(),
        })
    }

    /// DWARF x86_64 register number to Windows register number.
    fn win_reg(dw: u64) -> Option<u8> {
        Some(match dw {
            0 => 0, // rax
            1 => 2, // rdx
            2 => 1, // rcx
            3 => 3, // rbx
            4 => 6, // rsi
            5 => 7, // rdi
            6 => 5, // rbp
            7 => 4, // rsp
            8..=15 => dw as u8,
            _ => return None,
        })
    }

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Push(u8),
        Alloc(u32),
        /// Register saved at `[rsp + 8 * slot]` (UWOP_SAVE_NONVOL).
        Save(u8, u32),
        /// UWOP_SET_FPREG with rbp and this FrameOffset (units of 16).
        SetFp(u8),
    }

    /// Replays the CIE and FDE programs over the prologue and returns the
    /// unwind operations (prologue order, with code offsets) and the
    /// prologue length.
    ///
    /// Without a frame pointer, every CFA growth is a push (when a register
    /// is saved at the new top) or an allocation. With a frame pointer
    /// (`push rbp; mov rbp, rsp`, CFA = rbp + 16) the later pushes and the
    /// `sub rsp` do not show in the CFA, only the save slots do, so the
    /// frame is described the way Windows can unwind it from rbp: a
    /// frame offset below the lowest save, the saves relative to it, and
    /// an allocation that brings rsp back to rbp.
    fn translate(
        cie: &Cie,
        fde_insns: &[u8],
        func_len: u64,
    ) -> Result<(Vec<(u8, Op)>, u8), &'static str> {
        let mut prog = cie.insns.clone();
        let initial_len = prog.len();
        prog.extend_from_slice(fde_insns);
        let mut r = Reader {
            d: &prog,
            p: 0,
            base: 0,
        };
        let mut pc: u64 = 0;
        let mut cfa_reg = 7u64;
        let mut cfa_off: i64 = 8;
        // CFA growths on rsp: (pc, bytes, cfa offset after).
        let mut grows: Vec<(u64, u32, i64)> = vec![];
        // Saved registers: (DWARF register, cfa offset of the slot).
        let mut saves: Vec<(u64, i64)> = vec![];
        let mut fp = false;
        let mut end = 0u64;
        while r.p < prog.len() {
            let in_fde = r.p >= initial_len;
            let b = r.u8().ok_or("truncated CFI")?;
            let (hi, lo) = (b >> 6, (b & 0x3f) as u64);
            let mut save: Option<(u64, i64)> = None;
            match hi {
                1 => {
                    pc += lo * cie.code_align;
                    if pc >= func_len {
                        break;
                    }
                    continue;
                }
                2 => save = Some((lo, r.uleb().ok_or("cfi")? as i64 * cie.data_align)),
                3 => break, // restore: an epilogue
                _ => match b {
                    0x00 => {}
                    0x02 => pc += r.u8().ok_or("cfi")? as u64 * cie.code_align,
                    0x03 => pc += r.u16().ok_or("cfi")? as u64 * cie.code_align,
                    0x04 => pc += r.u32().ok_or("cfi")? as u64 * cie.code_align,
                    0x05 => {
                        let reg = r.uleb().ok_or("cfi")?;
                        save = Some((reg, r.uleb().ok_or("cfi")? as i64 * cie.data_align));
                    }
                    0x11 => {
                        let reg = r.uleb().ok_or("cfi")?;
                        save = Some((reg, r.sleb().ok_or("cfi")? * cie.data_align));
                    }
                    0x0a | 0x0b => break, // remember/restore state: past the prologue
                    0x0c => {
                        let reg = r.uleb().ok_or("cfi")?;
                        let n = r.uleb().ok_or("cfi")? as i64;
                        if in_fde && reg == 7 && (fp || n < cfa_off) {
                            break; // back on rsp: an epilogue
                        }
                        if in_fde && reg == 7 && n > cfa_off {
                            grows.push((pc, (n - cfa_off) as u32, n));
                        }
                        cfa_reg = reg;
                        cfa_off = n;
                    }
                    0x0d => cfa_reg = r.uleb().ok_or("cfi")?,
                    0x0e => {
                        let n = r.uleb().ok_or("cfi")? as i64;
                        if fp || n < cfa_off {
                            break; // an epilogue
                        }
                        if cfa_reg == 7 && n > cfa_off && in_fde {
                            grows.push((pc, (n - cfa_off) as u32, n));
                        }
                        cfa_off = n;
                    }
                    0x07 | 0x08 | 0x2e => {
                        r.uleb().ok_or("cfi")?;
                    }
                    0x09 => {
                        r.uleb().ok_or("cfi")?;
                        r.uleb().ok_or("cfi")?;
                    }
                    _ => return Err("unsupported CFI opcode"),
                },
            }
            if !in_fde {
                continue;
            }
            if let Some((reg, off)) = save {
                saves.push((reg, -off));
            }
            if cfa_reg == 6 && !fp {
                if cfa_off != 16 {
                    return Err("frame pointer at an unexpected offset");
                }
                fp = true;
                end = end.max(pc);
            } else if cfa_reg != 7 && cfa_reg != 6 {
                return Err("CFA on another register");
            }
            end = end.max(pc);
        }
        // A growth of 8 whose new top holds a saved register was a push.
        let mut out: Vec<(u64, Op)> = vec![];
        for &(gpc, n, after) in &grows {
            match saves.iter().position(|&(_, k)| k == after) {
                Some(i) if n == 8 => {
                    let (reg, _) = saves.remove(i);
                    out.push((gpc, Op::Push(win_reg(reg).ok_or("register")?)));
                }
                _ => out.push((gpc, Op::Alloc(n))),
            }
        }
        // The return address is not a register save.
        saves.retain(|&(reg, _)| reg != 16);
        if !fp && !saves.is_empty() {
            return Err("save that is not a push");
        }
        if fp {
            let fp_saves = saves;
            // Saves at rbp - d (d = cfa offset - 16).
            let max_d = fp_saves
                .iter()
                .map(|&(_, k)| k - 16)
                .max()
                .unwrap_or(0)
                .max(0);
            let fo = ((max_d + 15) / 16) as u32;
            if fo > 15 {
                return Err("saves too far below the frame pointer");
            }
            out.push((end, Op::Alloc(16 * fo)));
            for &(reg, k) in &fp_saves {
                let d = k - 16;
                if d <= 0 {
                    continue; // rbp itself, already a push
                }
                let w = win_reg(reg).ok_or("register")?;
                out.push((end, Op::Save(w, ((16 * fo as i64 - d) / 8) as u32)));
            }
            out.push((end, Op::SetFp(fo as u8)));
        }
        if end > 255 || out.iter().any(|o| o.0 > 255) {
            return Err("prologue longer than 255 bytes");
        }
        let out: Vec<(u8, Op)> = out
            .into_iter()
            .filter(|o| !matches!(o.1, Op::Alloc(0)))
            .map(|(p, o)| (p as u8, o))
            .collect();
        Ok((out, end as u8))
    }

    /// Encodes UNWIND_INFO: codes in reverse prologue order.
    fn unwind_info(ops: &[(u8, Op)], prologue: u8) -> Vec<u8> {
        let mut codes: Vec<u16> = vec![];
        let mut frame = 0u8;
        for &(off, op) in ops.iter().rev() {
            let c = |op: u16, info: u16| off as u16 | ((op | info << 4) << 8);
            match op {
                Op::Push(reg) => codes.push(c(0, reg as u16)),
                Op::SetFp(fo) => {
                    frame = fo;
                    codes.push(c(3, 0));
                }
                Op::Save(reg, slot) => {
                    codes.push(c(4, reg as u16));
                    codes.push(slot as u16);
                }
                Op::Alloc(n) if (8..=128).contains(&n) => codes.push(c(2, (n / 8 - 1) as u16)),
                Op::Alloc(n) if n / 8 <= 0xffff => {
                    codes.push(c(1, 0));
                    codes.push((n / 8) as u16);
                }
                Op::Alloc(n) => {
                    codes.push(c(1, 1));
                    codes.push(n as u16);
                    codes.push((n >> 16) as u16);
                }
            }
        }
        let fp = ops.iter().any(|o| matches!(o.1, Op::SetFp(_)));
        let mut v = vec![
            1u8,
            prologue,
            codes.len() as u8,
            if fp { 5 | (frame << 4) } else { 0 },
        ];
        for c in &codes {
            v.extend_from_slice(&c.to_le_bytes());
        }
        if codes.len() % 2 == 1 {
            v.extend_from_slice(&[0, 0]);
        }
        v
    }

    pub fn register(o: &Object) -> Result<usize, String> {
        let hdr = o
            .phdrs
            .iter()
            .find(|p| p.p_type == PT_GNU_EH_FRAME)
            .ok_or("no PT_GNU_EH_FRAME")?;
        // .eh_frame_hdr: version, eh_frame_ptr_enc, fde_count_enc,
        // table_enc, eh_frame_ptr, fde_count, table[(initial_loc, fde)].
        let h = o.bias + hdr.p_vaddr;
        // SAFETY: the header segment is mapped; the whole object span is
        // readable (code and read-only data).
        let mem = unsafe { core::slice::from_raw_parts(o.base as *const u8, o.size as usize) };
        let off = (h - o.base) as usize;
        let mut r = Reader {
            d: mem,
            p: off,
            base: o.base,
        };
        let (_v, ep_enc, cnt_enc, tab_enc) = (
            r.u8().ok_or("hdr")?,
            r.u8().ok_or("hdr")?,
            r.u8().ok_or("hdr")?,
            r.u8().ok_or("hdr")?,
        );
        r.ptr(ep_enc).ok_or("eh_frame_ptr")?;
        let count = r.ptr(cnt_enc).ok_or("fde_count")?;
        if tab_enc != 0x3b {
            return Err(format!("unexpected table encoding {tab_enc:#x}"));
        }
        let mut entries: Vec<IMAGE_RUNTIME_FUNCTION_ENTRY> = vec![];
        let mut infos: Vec<Vec<u8>> = vec![];
        let mut skipped: std::collections::HashMap<&'static str, usize> = Default::default();
        for _ in 0..count {
            let _loc = (r.u32().ok_or("tab")? as i32 as i64 + h as i64) as u64;
            let fde = (r.u32().ok_or("tab")? as i32 as i64 + h as i64) as u64;
            let mut f = Reader {
                d: mem,
                p: (fde - o.base) as usize,
                base: o.base,
            };
            let len = f.u32().ok_or("fde")? as usize;
            let start = f.p;
            let cie_ptr = f.p as u64 - f.u32().ok_or("fde")? as u64;
            let cie = parse_cie(mem, o.base, cie_ptr as usize).ok_or("cie")?;
            let pc_begin = f.ptr(cie.fde_enc).ok_or("pc_begin")?;
            let pc_range = f.ptr(cie.fde_enc & 0x0f).ok_or("pc_range")?;
            let alen = f.uleb().ok_or("aug")? as usize;
            f.p += alen;
            let insns = &mem[f.p..start + len];
            match translate(&cie, insns, pc_range) {
                Ok((ops, _)) if ops.is_empty() => {
                    // A leaf: Windows' default (return address at [rsp]).
                }
                Ok((ops, prologue)) => {
                    let info = unwind_info(&ops, prologue);
                    infos.push(info);
                    entries.push(IMAGE_RUNTIME_FUNCTION_ENTRY {
                        BeginAddress: (pc_begin - o.base) as u32,
                        EndAddress: (pc_begin + pc_range - o.base) as u32,
                        ..Default::default()
                    });
                }
                Err(e) => *skipped.entry(e).or_default() += 1,
            }
        }
        // UNWIND_INFO must lie within 4 GiB above the base: the loader
        // reserved UNWIND_SPACE after the image for it.
        let total: usize = infos.iter().map(|i| i.len() + 4).sum::<usize>() + entries.len() * 12;
        let total = (total + 4095) & !4095;
        if total as u64 > crate::loader::UNWIND_SPACE {
            return Err("unwind data larger than the reserved space".into());
        }
        let at = andruna_lx::mem::mmap(
            (o.base + o.size) as usize,
            total,
            andruna_lx::linux::mman::PROT_READ | andruna_lx::linux::mman::PROT_WRITE,
            andruna_lx::linux::mman::MAP_PRIVATE
                | andruna_lx::linux::mman::MAP_ANONYMOUS
                | andruna_lx::linux::mman::MAP_FIXED,
            -1,
            0,
        )
        .map(|a| a as u64)
        .ok();
        let at = at.ok_or("no room for unwind data near the object")?;
        let mut cur = at;
        for (e, info) in entries.iter_mut().zip(&infos) {
            // SAFETY: inside the mapping just made, sized for all infos.
            unsafe { core::ptr::copy_nonoverlapping(info.as_ptr(), cur as *mut u8, info.len()) };
            e.Anonymous.UnwindInfoAddress = (cur - o.base) as u32;
            cur += (info.len() as u64 + 3) & !3;
        }
        entries.sort_by_key(|e| e.BeginAddress);
        let table = cur as *mut IMAGE_RUNTIME_FUNCTION_ENTRY;
        // SAFETY: room for the table follows the infos in the mapping.
        unsafe { core::ptr::copy_nonoverlapping(entries.as_ptr(), table, entries.len()) };
        // SAFETY: a table of `entries.len()` records that stays mapped for
        // the life of the process.
        let ok = unsafe {
            RtlAddFunctionTable(core::slice::from_raw_parts(table, entries.len()), o.base)
        };
        if !ok {
            return Err("RtlAddFunctionTable failed".into());
        }
        if !skipped.is_empty() {
            eprintln!("[s8] {}: FDEs not translated: {skipped:?}", o.name);
        }
        Ok(entries.len())
    }
}
