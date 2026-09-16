//! A minimal MS-DOS environment, just enough to load and run PC-98 sound-driver
//! `.COM`/`.EXE` programs (and the resident selector stubs the hoot sets ship)
//! and let them install their timer ISR and play API.
//!
//! Scope is deliberately small and grown on demand: real-mode only, a flat
//! conventional-memory arena with a classic MCB chain, a PSP per program, the
//! INT 21h subset these drivers actually call, TSR (keep-process), interrupt
//! vector get/set, console input fed from a `conin` queue, and a virtual
//! working directory holding the set's files. Unimplemented calls are logged,
//! not fatal.
//!
//! Memory access goes through the CPU's shared flat image ([`X86Cpu::mem`]);
//! this module owns the DOS-visible *bookkeeping* (MCB chain, handles, PSP).

use std::collections::{BTreeMap, HashMap, VecDeque};

use hoot_cpu::{Reg16, X86Cpu};

// ---- Conventional-memory layout (segments) ------------------------------
// 0x0000..0x0040  real-mode IVT (256 * 4 bytes)
// 0x0040..0x0050  BIOS data area (stubbed)
// 0x0050..0x0060  DOS work area / SYSVARS (stubbed, ends ~linear 0x560)
// 0x0060..0x0080  INT trampoline table (256 * `F4 CF`, 512 bytes)
// 0x1000..0xA000  TPA (transient program area) — the MCB chain lives here
/// Segment of the 256-entry INT trampoline table (`F4 CF` pairs).
///
/// Placed at **0x0060** — the segment PC-98 MS-DOS points unhooked interrupt
/// vectors at (its dummy-handler area). Drivers detect "am I already resident?"
/// by testing whether their API vector still equals 0x60 (not yet installed) vs
/// a real handler; at any other segment such a driver wrongly concludes it is
/// already installed and takes its uninstall path (e.g. MUSIC.COM's `music -r`).
pub const TRAMP_SEG: u16 = 0x0060;
/// Offset within [`TRAMP_SEG`] of the sentinel a harness-initiated far call
/// returns to: a single `HLT` placed just past the 512-byte trampoline table
/// (linear 0x800), so it is never mistaken for an interrupt-vector trap
/// ([`trap_vector`](MiniDos::trap_vector) only recognizes `ip < 512`). Used by
/// the device-driver INIT far calls — see `Engine::call_far`.
pub const CALL_RET_OFF: u16 = 0x0200;
/// Segment of the DOS internal structures (List of Lists etc.), in the free
/// DOS work area (linear 0x500..0x7FF, between the BIOS data area and the
/// trampoline table).
const SYSVARS_SEG: u16 = 0x0050;
/// Offset of the List of Lists within [`SYSVARS_SEG`] (leaves room for the
/// `LoL-2` first-MCB field that some resident scans read).
const LOL_OFF: u16 = 0x0010;
/// First MCB paragraph of the transient program area.
pub const ARENA_START: u16 = 0x1000;
/// One past the last usable paragraph (640 KB conventional top for our purposes).
pub const ARENA_END: u16 = 0xA000;
/// Placeholder owner for a program's environment block, held between allocating
/// it and allocating the PSP that will own it. Any non-zero value does: owner
/// zero means "free", which would let the PSP allocation take the block back.
const ENV_PENDING_OWNER: u16 = 0xFFFF;

/// The DOS path a program was loaded from, as its environment block records it.
/// `default_ext` supplies the extension for a bare command name (`opndrv`), and
/// any directory part of the name is dropped — the virtual CWD is the set folder.
fn program_path(name: &str, default_ext: &str) -> String {
    let up = name.to_uppercase();
    let base = up.rsplit(['\\', '/', ':']).next().unwrap_or(&up);
    match base.contains('.') {
        true => format!("C:\\{base}"),
        false => format!("C:\\{base}.{default_ext}"),
    }
}

/// The `HLT` byte that fronts every trampoline entry (traps to the harness).
const HLT: u8 = 0xF4;
/// The `IRET` byte the harness lands on after servicing a trapped INT.
const IRET: u8 = 0xCF;

/// Outcome of running one program to completion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExecResult {
    /// Program terminated (INT 20h / INT 21h AH=4Ch); its memory is freed.
    Terminated(u8),
    /// Program went resident (INT 21h AH=31h / INT 27h); memory is kept.
    Resident,
}

struct OpenFile {
    name: String,
    pos: usize,
}

/// What the guest asked the PC-98 timer BIOS (INT 1Ch) to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiosTimerReq {
    /// Call `seg:off` once, `ticks` BIOS timer ticks from now.
    Arm { seg: u16, off: u16, ticks: u16 },
    /// Drop the pending one-shot, wherever it has got to.
    Cancel,
}

/// A minimal DOS environment shared across a set's shell-command chain.
pub struct MiniDos {
    /// Virtual working directory: UPPERCASE filename -> contents.
    files: BTreeMap<String, Vec<u8>>,
    /// Open file handles (5.. ; 0-4 are the standard devices).
    handles: HashMap<u16, OpenFile>,
    next_handle: u16,
    /// Console-input bytes queued for AH=01/06/07/08 (the `conin` selection).
    conin: VecDeque<u8>,
    /// PSP segment of the program currently executing.
    psp_seg: u16,
    /// Disk transfer address (seg, off) — default PSP:0x80.
    dta: (u16, u16),
    /// Interrupt vectors the running program installed via AH=25h (diagnostics).
    pub installed_vectors: BTreeMap<u8, (u16, u16)>,
    /// INT/AH combinations we do not implement yet (diagnostics).
    pub unimpl: BTreeMap<(u8, u8), u64>,
    /// Console output captured from AH=02/06/09 (diagnostics / debugging).
    pub con_out: Vec<u8>,
    /// Read calls (AH=3Fh): (handle, bytes returned), for diagnostics — reveals
    /// whether a funcvect stub actually read the song data we handed it.
    pub read_log: Vec<(u16, usize)>,
    /// The guest's latest PC-98 timer-BIOS request, waiting to be picked up.
    /// The harness drains this every step, so a cancel has to travel the same
    /// path as an arm — clearing the field would only clear a request the
    /// harness has already taken, leaving a cancelled callback to fire.
    pub bios_timer: Option<BiosTimerReq>,
}

// ---- little-endian helpers over the flat image --------------------------
fn rd8(mem: &[u8], lin: usize) -> u8 {
    mem[lin]
}
fn rd16(mem: &[u8], lin: usize) -> u16 {
    mem[lin] as u16 | ((mem[lin + 1] as u16) << 8)
}
fn wr8(mem: &mut [u8], lin: usize, v: u8) {
    mem[lin] = v;
}
fn wr16(mem: &mut [u8], lin: usize, v: u16) {
    mem[lin] = v as u8;
    mem[lin + 1] = (v >> 8) as u8;
}

fn lin(seg: u16, off: u16) -> usize {
    ((seg as usize) << 4) + off as usize
}

impl MiniDos {
    pub fn new() -> Self {
        MiniDos {
            files: BTreeMap::new(),
            handles: HashMap::new(),
            next_handle: 5,
            conin: VecDeque::new(),
            psp_seg: 0,
            dta: (0, 0),
            installed_vectors: BTreeMap::new(),
            unimpl: BTreeMap::new(),
            con_out: Vec::new(),
            read_log: Vec::new(),
            bios_timer: None,
        }
    }

    /// Add a file to the virtual working directory (name matched case-insensitively).
    pub fn add_file(&mut self, name: &str, data: Vec<u8>) {
        self.files.insert(name.to_uppercase(), data);
    }

    /// Patch one byte of an already-added file (for `<rom type="binary">`).
    /// `upper_name` must be the UPPERCASE file name; out-of-range offsets are
    /// ignored.
    pub fn patch_file(&mut self, upper_name: &str, off: usize, val: u8) {
        if let Some(d) = self.files.get_mut(upper_name) {
            if off < d.len() {
                d[off] = val;
            }
        }
    }

    /// Look up a file case-insensitively, honoring a default extension.
    fn find_file(&self, name: &str) -> Option<&Vec<u8>> {
        let up = name.to_uppercase();
        if let Some(d) = self.files.get(&up) {
            return Some(d);
        }
        // Strip any drive/path and retry on the basename.
        let base = up.rsplit(['\\', '/', ':']).next().unwrap_or(&up).to_string();
        self.files.get(&base)
    }

    /// Queue console-input bytes for the selector stub (title `conin`).
    pub fn queue_conin(&mut self, bytes: &[u8]) {
        self.conin.extend(bytes.iter().copied());
    }

    /// Bind an already-materialized file to a specific DOS handle, as if hoot
    /// had opened it and left the handle open for a resident stub to read (the
    /// funcvect glue reads its song data from a preset handle). `name` is matched
    /// case-insensitively.
    pub fn set_handle(&mut self, handle: u16, name: &str) {
        self.handles.insert(handle, OpenFile { name: name.to_uppercase(), pos: 0 });
        if handle >= self.next_handle {
            self.next_handle = handle + 1;
        }
    }

    /// Bind `handle` to a small virtual file whose *content is `text`* — used for
    /// `conin` roms. hoot presents a `conin` rom's referenced name as a FILENAME
    /// string on its DOS handle: the usd_98/mlalf_98 driver families read that
    /// handle as an ASCIIZ filename, open it, and AH=4B03 overlay-load it as
    /// their FM engine (the engine file is separately materialized on disk so the
    /// open-by-name succeeds). Reading returns exactly `text.len()` bytes so the
    /// driver's own NUL-termination / pre-zeroed buffer terminates the name.
    pub fn set_handle_text(&mut self, handle: u16, text: &str) {
        let key = format!("\u{1}CONIN{handle:02X}");
        self.files.insert(key.clone(), text.as_bytes().to_vec());
        self.handles.insert(handle, OpenFile { name: key, pos: 0 });
        if handle >= self.next_handle {
            self.next_handle = handle + 1;
        }
    }

    // ---- IVT + trampolines ----------------------------------------------

    /// Point every IVT entry at its trampoline (`F4 CF`) so all software
    /// interrupts trap to the harness until a program installs its own handler.
    pub fn install_trampolines(&self, mem: &mut [u8]) {
        let base = lin(TRAMP_SEG, 0);
        for v in 0..256usize {
            wr8(mem, base + v * 2, HLT);
            wr8(mem, base + v * 2 + 1, IRET);
            // IVT[v] = TRAMP_SEG:(v*2)
            wr16(mem, v * 4, (v * 2) as u16);
            wr16(mem, v * 4 + 2, TRAMP_SEG);
        }
    }

    /// If CS:IP sits on a trampoline `HLT`, return the interrupt vector it fronts.
    pub fn trap_vector(&self, cs: u16, ip: u16) -> Option<u8> {
        if cs == TRAMP_SEG && ip < 512 && ip % 2 == 0 {
            Some((ip / 2) as u8)
        } else {
            None
        }
    }

    // ---- MCB arena ------------------------------------------------------

    /// Lay down a single free block spanning the whole TPA.
    pub fn init_arena(&self, mem: &mut [u8]) {
        let size = ARENA_END - ARENA_START - 1; // paragraphs after the MCB header
        self.write_mcb(mem, ARENA_START, b'Z', 0, size);
    }

    /// Lay down a minimal, self-consistent set of DOS internal structures so
    /// programs that inspect DOS internals don't walk off into garbage. Chiefly
    /// the List of Lists (INT 21h AH=52h): `LoL-2` points at the real first MCB,
    /// and `LoL+4` points at an empty, immediately-terminating far-linked chain
    /// so a resident-driver self-scan concludes "not present" and returns
    /// cleanly. (Reverse-engineered from PMD.COM's MCB/SFT walk.)
    pub fn install_dos_structures(&self, mem: &mut [u8]) {
        let seg = SYSVARS_SEG;
        for off in 0..0x60u16 {
            wr8(mem, lin(seg, off), 0);
        }
        // LoL-2 = segment of the first memory control block.
        wr16(mem, lin(seg, LOL_OFF - 2), ARENA_START);
        // LoL+4 = far pointer to a chain node at seg:0x40.
        wr16(mem, lin(seg, LOL_OFF + 4), 0x0040);
        wr16(mem, lin(seg, LOL_OFF + 6), seg);
        // The node: next-pointer offset 0xFFFF (list terminator), size 1, and a
        // zeroed name area (never matches a real program name).
        wr16(mem, lin(seg, 0x40), 0xFFFF);
        wr16(mem, lin(seg, 0x42), seg);
        wr16(mem, lin(seg, 0x44), 0x0001);
    }

    /// Walk the MCB chain for diagnostics: (mcb_seg, owner_psp, size_paras).
    pub fn mcb_chain(&self, mem: &[u8]) -> Vec<(u16, u16, u16)> {
        let mut out = Vec::new();
        let mut seg = ARENA_START;
        loop {
            let l = lin(seg, 0);
            let sig = rd8(mem, l);
            if sig != b'M' && sig != b'Z' {
                break;
            }
            let owner = rd16(mem, l + 1);
            let size = rd16(mem, l + 3);
            out.push((seg, owner, size));
            if sig == b'Z' || out.len() > 64 {
                break;
            }
            seg = seg + 1 + size;
        }
        out
    }

    fn write_mcb(&self, mem: &mut [u8], seg: u16, sig: u8, owner: u16, size: u16) {
        let l = lin(seg, 0);
        wr8(mem, l, sig); // 'M' (member) or 'Z' (last)
        wr16(mem, l + 1, owner); // owner PSP (0 = free)
        wr16(mem, l + 3, size); // size in paragraphs
        for b in &mut mem[l + 5..l + 16] {
            *b = 0;
        }
    }

    /// Allocate `paras` paragraphs; returns the data segment (MCB+1) or None.
    /// First-fit over the chain; splits the block when larger than requested.
    /// Size (in paragraphs) of the largest free block in the arena — what DOS
    /// AH=48/AH=4A return in BX when the request cannot be satisfied. Drivers
    /// probe available memory with the ubiquitous "allocate 0xFFFF, read back BX,
    /// allocate BX" idiom, so BX must be right or they spin/bail.
    pub fn max_free_block(&self, mem: &[u8]) -> u16 {
        let mut seg = ARENA_START;
        let mut best = 0u16;
        loop {
            let l = lin(seg, 0);
            let sig = rd8(mem, l);
            let block_owner = rd16(mem, l + 1);
            let block_size = rd16(mem, l + 3);
            if block_owner == 0 {
                best = best.max(block_size);
            }
            if sig == b'Z' {
                return best;
            }
            seg = seg + 1 + block_size;
        }
    }

    pub fn alloc(&self, mem: &mut [u8], paras: u16, owner: u16) -> Option<u16> {
        let mut seg = ARENA_START;
        loop {
            let l = lin(seg, 0);
            let sig = rd8(mem, l);
            let block_owner = rd16(mem, l + 1);
            let block_size = rd16(mem, l + 3);
            let is_last = sig == b'Z';
            if block_owner == 0 && block_size >= paras {
                // Take this block; split off the remainder if worthwhile.
                if block_size > paras + 1 {
                    let rem_seg = seg + 1 + paras;
                    let rem_size = block_size - paras - 1;
                    self.write_mcb(mem, rem_seg, if is_last { b'Z' } else { b'M' }, 0, rem_size);
                    self.write_mcb(mem, seg, b'M', owner, paras);
                } else {
                    self.write_mcb(mem, seg, sig, owner, block_size);
                }
                return Some(seg + 1);
            }
            if is_last {
                return None;
            }
            seg = seg + 1 + block_size;
        }
    }

    /// Whether `data_seg` names a block inside the transient program area (and
    /// so its MCB header at `data_seg-1` is real). Programs sometimes free or
    /// resize segments outside the arena (e.g. a null/absent environment); those
    /// are quietly ignored rather than corrupting the low-memory MCB math.
    fn in_arena(seg: u16) -> bool {
        seg > ARENA_START && seg < ARENA_END
    }

    /// Free a block by data segment (MCB = seg-1). Coalescing is left to the
    /// next alloc's first-fit; we just mark it free.
    pub fn free(&self, mem: &mut [u8], data_seg: u16) {
        if !Self::in_arena(data_seg) {
            return;
        }
        let mcb = data_seg - 1;
        let l = lin(mcb, 0);
        wr16(mem, l + 1, 0); // owner = 0 (free)
        self.coalesce(mem);
    }

    /// Merge adjacent free blocks so a later large alloc can succeed.
    fn coalesce(&self, mem: &mut [u8]) {
        let mut seg = ARENA_START;
        loop {
            let l = lin(seg, 0);
            let sig = rd8(mem, l);
            let owner = rd16(mem, l + 1);
            let size = rd16(mem, l + 3);
            if sig != b'M' && sig != b'Z' {
                return;
            }
            if sig == b'Z' {
                return;
            }
            let next = seg + 1 + size;
            let nl = lin(next, 0);
            let nsig = rd8(mem, nl);
            let nowner = rd16(mem, nl + 1);
            let nsize = rd16(mem, nl + 3);
            if owner == 0 && nowner == 0 {
                // Absorb the next block into this one.
                let merged = size + 1 + nsize;
                self.write_mcb(mem, seg, nsig, 0, merged);
                continue; // retry from the same seg in case of a run of frees
            }
            seg = next;
        }
    }

    /// Resize the block owning `data_seg` to `paras` (AH=4Ah). Returns Ok, or
    /// Err(max_available) if growth is blocked by the following block.
    pub fn resize(&self, mem: &mut [u8], data_seg: u16, paras: u16) -> Result<(), u16> {
        if !Self::in_arena(data_seg) {
            return Ok(()); // resizing a non-arena block: nothing to do
        }
        let mcb = data_seg - 1;
        let l = lin(mcb, 0);
        let sig = rd8(mem, l);
        let owner = rd16(mem, l + 1);
        let size = rd16(mem, l + 3);
        if paras <= size {
            // Shrink: split the tail into a free block.
            if size > paras {
                let rem_seg = mcb + 1 + paras;
                let rem_size = size - paras - 1;
                if size >= paras + 1 {
                    self.write_mcb(mem, rem_seg, if sig == b'Z' { b'Z' } else { b'M' }, 0, rem_size);
                    self.write_mcb(mem, mcb, b'M', owner, paras);
                }
            }
            return Ok(());
        }
        // Grow into the following block if it is free and big enough.
        if sig == b'Z' {
            return Err(size);
        }
        let next = mcb + 1 + size;
        let nl = lin(next, 0);
        let nowner = rd16(mem, nl + 1);
        let nsize = rd16(mem, nl + 3);
        let nsig = rd8(mem, nl);
        let combined = size + 1 + nsize;
        if nowner == 0 && combined >= paras {
            if combined > paras + 1 {
                let rem_seg = mcb + 1 + paras;
                let rem_size = combined - paras - 1;
                self.write_mcb(mem, rem_seg, if nsig == b'Z' { b'Z' } else { b'M' }, 0, rem_size);
                self.write_mcb(mem, mcb, b'M', owner, paras);
            } else {
                self.write_mcb(mem, mcb, nsig, owner, combined);
            }
            Ok(())
        } else {
            Err(size + if nowner == 0 { 1 + nsize } else { 0 })
        }
    }

    // ---- PSP ------------------------------------------------------------

    /// Build a Program Segment Prefix at `psp_seg` with the given memory top and
    /// command tail. Returns nothing; the PSP is written into `mem`.
    fn build_psp(&self, mem: &mut [u8], psp_seg: u16, mem_top_para: u16, tail: &[u8], env_seg: u16) {
        let p = lin(psp_seg, 0);
        for b in &mut mem[p..p + 256] {
            *b = 0;
        }
        // INT 20h at PSP:0000.
        wr8(mem, p + 0x00, 0xCD);
        wr8(mem, p + 0x01, 0x20);
        // Segment of the first paragraph beyond the program's memory.
        wr16(mem, p + 0x02, mem_top_para);
        // Environment segment.
        wr16(mem, p + 0x2C, env_seg);
        // Command tail at 0x80: length byte, bytes, CR.
        let n = tail.len().min(126);
        wr8(mem, p + 0x80, n as u8);
        for (i, &b) in tail.iter().take(n).enumerate() {
            wr8(mem, p + 0x81 + i, b);
        }
        wr8(mem, p + 0x81 + n, 0x0D);
    }

    /// Allocate and fill the environment block DOS hands a child process.
    ///
    /// Layout is exactly MS-DOS's: the variable strings (each ASCIIZ), one more
    /// NUL closing the list, a `0x0001` count word, then the program's own path
    /// as ASCIIZ. The block is owned by `owner` so the program can free it.
    ///
    /// This is not decoration. A program that wants to know where it was loaded
    /// from walks this layout — scan for the `\0\0` that ends the variable list,
    /// copy what follows into its PSP, then release the block. With PSP:0x2C
    /// left at zero that walk starts from segment 0xFFFF, reads a garbage block
    /// length out of `[0xFFFF:0003]`, and `rep movsb`s kilobytes of nonsense
    /// over the program's own code. FUGA System's OPNDRV 2.04 and later do
    /// precisely this, and so overwrote themselves and fell into the PSP's
    /// INT 20h instead of going resident — 27 sets that never played a note.
    ///
    /// Keep the block tight: the copy length is whatever remains of it after
    /// the variable list, and the destination is inside the caller's PSP.
    fn alloc_env(&self, mem: &mut [u8], owner: u16, path: &str) -> Option<u16> {
        let mut env: Vec<u8> = Vec::new();
        env.extend_from_slice(b"COMSPEC=C:\\COMMAND.COM\0");
        env.push(0); // end of the variable list
        env.extend_from_slice(&[0x01, 0x00]); // trailing-name count
        env.extend_from_slice(path.as_bytes());
        env.push(0);
        let paras = env.len().div_ceil(16) as u16;
        let seg = self.alloc(mem, paras, owner)?;
        let base = lin(seg, 0);
        for b in &mut mem[base..base + paras as usize * 16] {
            *b = 0;
        }
        mem[base..base + env.len()].copy_from_slice(&env);
        Some(seg)
    }

    // ---- program loaders ------------------------------------------------

    /// Load a `.COM` image: PSP + image at PSP:0x100, all segregs = PSP,
    /// SP just below the 64 KB (or block) top. Returns the PSP segment.
    pub fn load_com(
        &mut self,
        cpu: &mut dyn X86Cpu,
        name: &str,
        image: &[u8],
        tail: &[u8],
    ) -> anyhow::Result<u16> {
        // The environment goes below the PSP, as DOS builds it: copied first,
        // then the program loaded above it. Owner is patched to the PSP once we
        // have one (a zero owner would mark the block free and hand it straight
        // back to the .COM allocation below).
        let env_seg = self
            .alloc_env(cpu.mem(), ENV_PENDING_OWNER, &program_path(name, "COM"))
            .ok_or_else(|| anyhow::anyhow!("out of memory building environment for {name:?}"))?;
        // A .COM wants a full 64 KB segment; allocate the largest block we can,
        // capped at 0x1000 paragraphs (64 KB), like DOS hands a COM everything.
        let need = 0x1000u16; // 64 KB in paragraphs
        let psp_seg = self
            .alloc(cpu.mem(), need, 0)
            .ok_or_else(|| anyhow::anyhow!("out of memory loading .COM ({} bytes)", image.len()))?;
        let owner = psp_seg;
        // Re-stamp the block's owner to itself (PSP).
        let mcb = psp_seg - 1;
        wr16(cpu.mem(), lin(mcb, 0) + 1, owner);
        wr16(cpu.mem(), lin(env_seg - 1, 0) + 1, psp_seg);

        let mem_top = psp_seg + need;
        self.build_psp(cpu.mem(), psp_seg, mem_top, tail, env_seg);

        // Load image at PSP:0x100.
        let load = lin(psp_seg, 0x100);
        if 0x100 + image.len() > 0x10000 {
            anyhow::bail!(".COM image too large for one segment: {} bytes", image.len());
        }
        cpu.mem()[load..load + image.len()].copy_from_slice(image);

        self.psp_seg = psp_seg;
        self.dta = (psp_seg, 0x80);
        cpu.set_reg16(Reg16::Ds, psp_seg);
        cpu.set_reg16(Reg16::Es, psp_seg);
        cpu.set_ss_sp(psp_seg, 0xFFFE);
        // AX = 0 (both FCB drive checks "valid").
        cpu.set_reg16(Reg16::Ax, 0);
        cpu.set_cs_ip(psp_seg, 0x100);
        Ok(psp_seg)
    }

    /// Load an `.EXE` (MZ) image: parse the header, place the load module, apply
    /// relocations, set CS:IP / SS:SP from the (relocated) header. Returns PSP.
    pub fn load_exe(
        &mut self,
        cpu: &mut dyn X86Cpu,
        name: &str,
        image: &[u8],
        tail: &[u8],
    ) -> anyhow::Result<u16> {
        if image.len() < 0x20 || &image[0..2] != b"MZ" {
            anyhow::bail!("not an MZ executable");
        }
        let bytes_last_page = rd16(image, 0x02) as usize;
        let pages = rd16(image, 0x04) as usize;
        let nreloc = rd16(image, 0x06) as usize;
        let hdr_paras = rd16(image, 0x08) as usize;
        let min_alloc = rd16(image, 0x0A) as usize;
        let init_ss = rd16(image, 0x0E);
        let init_sp = rd16(image, 0x10);
        let init_ip = rd16(image, 0x14);
        let init_cs = rd16(image, 0x16);
        let reloc_off = rd16(image, 0x18) as usize;

        let hdr_size = hdr_paras * 16;
        let mut image_size = pages * 512;
        if bytes_last_page != 0 {
            image_size = image_size - 512 + bytes_last_page;
        }
        let load_size = image_size.saturating_sub(hdr_size);

        // The environment goes below the PSP, as DOS builds it (see `alloc_env`).
        let env_seg = self
            .alloc_env(cpu.mem(), ENV_PENDING_OWNER, &program_path(name, "EXE"))
            .ok_or_else(|| anyhow::anyhow!("out of memory building environment for {name:?}"))?;

        // Allocate PSP + load module + requested BSS.
        let load_paras = (load_size + 15) / 16;
        let need = (0x10 + load_paras + min_alloc) as u16; // PSP is 0x10 paras
        let psp_seg = self
            .alloc(cpu.mem(), need.max(0x11), 0)
            .ok_or_else(|| anyhow::anyhow!("out of memory loading .EXE"))?;
        let mcb = psp_seg - 1;
        wr16(cpu.mem(), lin(mcb, 0) + 1, psp_seg);
        wr16(cpu.mem(), lin(env_seg - 1, 0) + 1, psp_seg);

        let load_seg = psp_seg + 0x10; // program loads just past the PSP
        let mem_top = psp_seg + need;
        self.build_psp(cpu.mem(), psp_seg, mem_top, tail, env_seg);

        // Copy the load module.
        let dst = lin(load_seg, 0);
        let src_end = (hdr_size + load_size).min(image.len());
        let copy = &image[hdr_size..src_end];
        cpu.mem()[dst..dst + copy.len()].copy_from_slice(copy);

        // Apply relocations: each entry (off, seg) -> add load_seg to the word.
        for i in 0..nreloc {
            let e = reloc_off + i * 4;
            let off = rd16(image, e);
            let seg = rd16(image, e + 2);
            let target = lin(load_seg.wrapping_add(seg), off);
            let cur = rd16(cpu.mem(), target);
            wr16(cpu.mem(), target, cur.wrapping_add(load_seg));
        }

        self.psp_seg = psp_seg;
        self.dta = (psp_seg, 0x80);
        cpu.set_reg16(Reg16::Ds, psp_seg);
        cpu.set_reg16(Reg16::Es, psp_seg);
        cpu.set_ss_sp(load_seg.wrapping_add(init_ss), init_sp);
        cpu.set_reg16(Reg16::Ax, 0);
        cpu.set_cs_ip(load_seg.wrapping_add(init_cs), init_ip);
        Ok(psp_seg)
    }

    /// INT 21h AH=4Bh AL=03 — load an overlay into a caller-provided segment.
    ///
    /// Unlike AL=00 (load-and-execute a child), an overlay load does NOT create a
    /// PSP, allocate memory, or transfer control: DOS just copies the load module
    /// into `load_seg` and applies relocations using the caller-supplied factor;
    /// the caller then `call`s the entry itself. This is how several PC-98 sound
    /// stubs pull in their real driver (e.g. `ranma.com` loads `OPENING.APL`).
    /// A non-MZ image is copied verbatim (a raw `.COM`-style overlay).
    pub fn load_overlay(&self, cpu: &mut dyn X86Cpu, image: &[u8], load_seg: u16, reloc: u16) {
        if image.len() >= 0x20 && &image[0..2] == b"MZ" {
            let bytes_last_page = rd16(image, 0x02) as usize;
            let pages = rd16(image, 0x04) as usize;
            let nreloc = rd16(image, 0x06) as usize;
            let hdr_paras = rd16(image, 0x08) as usize;
            let reloc_off = rd16(image, 0x18) as usize;
            let hdr_size = hdr_paras * 16;
            let mut image_size = pages * 512;
            if bytes_last_page != 0 {
                image_size = image_size - 512 + bytes_last_page;
            }
            let load_size = image_size.saturating_sub(hdr_size);
            let dst = lin(load_seg, 0);
            let src_end = (hdr_size + load_size).min(image.len());
            let copy = &image[hdr_size..src_end];
            cpu.mem()[dst..dst + copy.len()].copy_from_slice(copy);
            for i in 0..nreloc {
                let e = reloc_off + i * 4;
                if e + 4 > image.len() {
                    break;
                }
                let off = rd16(image, e);
                let seg = rd16(image, e + 2);
                let target = lin(load_seg.wrapping_add(seg), off);
                let cur = rd16(cpu.mem(), target);
                wr16(cpu.mem(), target, cur.wrapping_add(reloc));
            }
        } else {
            let dst = lin(load_seg, 0);
            let end = dst + image.len();
            cpu.mem()[dst..end].copy_from_slice(image);
        }
    }

    /// Return a copy of a materialized file's bytes (case-insensitive, basename
    /// matched). Used to fetch a `.SYS` device-driver image for loading.
    pub fn file_bytes(&self, name: &str) -> Option<Vec<u8>> {
        self.find_file(name).cloned()
    }

    /// Load a DOS character-device driver into a freshly allocated, self-owned
    /// block and return the segment whose offset 0 holds the device header.
    ///
    /// A raw `.SYS` is a headerless image placed at paragraph:0000 (the device
    /// header IS the first bytes). An `.EXE`-format device (`MDR.EXE`) is an MZ
    /// image whose *load module* begins with the device header; it is relocated
    /// like a program but with no PSP/entry transfer (the initial CS:IP is
    /// irrelevant — the device is entered only through its STRATEGY/INTERRUPT
    /// offsets, which are relative to the load-module segment). Either way DOS
    /// then calls STRATEGY/INTERRUPT for INIT. The block carries headroom for
    /// the buffers the driver carves out during INIT and is never freed, so
    /// later program loads sit after it; the caller should [`resize`] it down to
    /// the INIT break address afterward.
    pub fn load_device_image(&self, cpu: &mut dyn X86Cpu, image: &[u8]) -> anyhow::Result<u16> {
        if image.len() >= 0x20 && &image[0..2] == b"MZ" {
            // MZ device: relocate the load module to seg:0 (device header there).
            let bytes_last_page = rd16(image, 0x02) as usize;
            let pages = rd16(image, 0x04) as usize;
            let nreloc = rd16(image, 0x06) as usize;
            let hdr_paras = rd16(image, 0x08) as usize;
            let reloc_off = rd16(image, 0x18) as usize;
            let hdr_size = hdr_paras * 16;
            let mut image_size = pages * 512;
            if bytes_last_page != 0 {
                image_size = image_size - 512 + bytes_last_page;
            }
            let load_size = image_size.saturating_sub(hdr_size);
            let load_paras = ((load_size + 15) / 16) as u16;
            let need = load_paras.saturating_add(0x1000);
            let seg = self
                .alloc(cpu.mem(), need.max(0x11), 0)
                .ok_or_else(|| anyhow::anyhow!("out of memory loading device driver"))?;
            let mcb = seg - 1;
            wr16(cpu.mem(), lin(mcb, 0) + 1, seg);
            let dst = lin(seg, 0);
            let src_end = (hdr_size + load_size).min(image.len());
            let copy = &image[hdr_size..src_end];
            cpu.mem()[dst..dst + copy.len()].copy_from_slice(copy);
            for i in 0..nreloc {
                let e = reloc_off + i * 4;
                if e + 4 > image.len() {
                    break;
                }
                let off = rd16(image, e);
                let sg = rd16(image, e + 2);
                let target = lin(seg.wrapping_add(sg), off);
                let cur = rd16(cpu.mem(), target);
                wr16(cpu.mem(), target, cur.wrapping_add(seg));
            }
            return Ok(seg);
        }
        // Raw `.SYS`: place the headerless image at paragraph:0.
        let paras = ((image.len() + 15) / 16) as u16;
        let need = paras.saturating_add(0x1000);
        let seg = self
            .alloc(cpu.mem(), need, 0)
            .ok_or_else(|| anyhow::anyhow!("out of memory loading device driver"))?;
        let mcb = seg - 1;
        wr16(cpu.mem(), lin(mcb, 0) + 1, seg);
        let dst = lin(seg, 0);
        cpu.mem()[dst..dst + image.len()].copy_from_slice(image);
        Ok(seg)
    }

    /// Resolve a shell command's program name to its image bytes and kind.
    /// Tries `name`, `name.COM`, `name.EXE` (case-insensitive).
    pub fn resolve_program(&self, name: &str) -> Option<(Vec<u8>, ProgKind)> {
        let up = name.to_uppercase();
        let candidates = if up.contains('.') {
            vec![up.clone()]
        } else {
            vec![format!("{up}.COM"), format!("{up}.EXE"), up.clone()]
        };
        for c in candidates {
            if let Some(d) = self.find_file(&c) {
                let kind = if d.len() >= 2 && &d[0..2] == b"MZ" {
                    ProgKind::Exe
                } else {
                    ProgKind::Com
                };
                return Some((d.clone(), kind));
            }
        }
        None
    }
}

// ---- byte-register helpers ---------------------------------------------
fn ah(cpu: &dyn X86Cpu) -> u8 {
    (cpu.reg16(Reg16::Ax) >> 8) as u8
}
fn al(cpu: &dyn X86Cpu) -> u8 {
    cpu.reg16(Reg16::Ax) as u8
}
fn set_al(cpu: &mut dyn X86Cpu, v: u8) {
    let ax = cpu.reg16(Reg16::Ax);
    cpu.set_reg16(Reg16::Ax, (ax & 0xFF00) | v as u16);
}
fn set_cf(cpu: &mut dyn X86Cpu, on: bool) {
    let mut f = cpu.reg16(Reg16::Flags);
    if on {
        f |= hoot_cpu::flag::CF;
    } else {
        f &= !hoot_cpu::flag::CF;
    }
    cpu.set_reg16(Reg16::Flags, f);
}

/// How long a single program may run before we declare it hung (CPU cycles).
const EXEC_BUDGET: u64 = 60_000_000;
/// Cycles per `run` burst between INT-trap checks.
const BURST: u32 = 100_000;

impl MiniDos {
    /// Run one shell command's program to completion (termination or TSR).
    /// `io` handles port access the program performs during setup.
    pub fn exec(
        &mut self,
        cpu: &mut dyn X86Cpu,
        io: &mut dyn hoot_cpu::IoBus,
        cmdline: &str,
    ) -> anyhow::Result<ExecResult> {
        let cmd = cmdline.trim();
        let (name, tail) = match cmd.split_once(char::is_whitespace) {
            Some((n, t)) => (n, t.trim_start()),
            None => (cmd, ""),
        };
        let (image, kind) = self
            .resolve_program(name)
            .ok_or_else(|| anyhow::anyhow!("shell program {name:?} not found in set"))?;
        match kind {
            ProgKind::Com => self.load_com(cpu, name, &image, tail.as_bytes())?,
            ProgKind::Exe => self.load_exe(cpu, name, &image, tail.as_bytes())?,
        };

        let mut total: u64 = 0;
        loop {
            let (cycles, stop) = cpu.run(io, BURST);
            total += cycles as u64;
            match stop {
                hoot_cpu::Stop::Halted => {
                    let cs = cpu.reg16(Reg16::Cs);
                    let ip = cpu.reg16(Reg16::Ip);
                    if let Some(vec) = self.trap_vector(cs, ip) {
                        if let Some(res) = self.service_int(cpu, vec) {
                            return Ok(res);
                        }
                        // Serviced: return to the caller ourselves, propagating
                        // the DOS status flags (CF/ZF) our handler set. A plain
                        // trampoline IRET would restore the caller's *old* flags
                        // off the stack and drop the return status.
                        self.iret_return(cpu);
                    } else {
                        // A real HLT with no timer running would hang forever;
                        // treat it as a NOP so setup keeps progressing.
                        cpu.set_reg16(Reg16::Ip, ip + 1);
                    }
                }
                hoot_cpu::Stop::Clocks => {}
            }
            if total > EXEC_BUDGET {
                anyhow::bail!("program {name:?} did not terminate within budget");
            }
        }
    }

    /// Service a trapped software interrupt. Returns `Some` if the program is
    /// finished (terminated or resident), `None` if serviced and it continues.
    pub fn service_int(&mut self, cpu: &mut dyn X86Cpu, vec: u8) -> Option<ExecResult> {
        match vec {
            0x20 => return Some(ExecResult::Terminated(0)),
            0x27 => return Some(ExecResult::Resident),
            0x21 => return self.int21(cpu),
            0x18 => self.int18(cpu),
            0x1C => self.int1c(cpu),
            _ => {
                *self.unimpl.entry((vec, ah(cpu))).or_default() += 1;
            }
        }
        None
    }

    /// PC-98 keyboard/CRT BIOS (INT 18h). Enough to keep a resident stub's idle
    /// poll loop happy: report "no key ready" for the sense/read functions,
    /// no-op the rest.
    fn int18(&mut self, cpu: &mut dyn X86Cpu) {
        match ah(cpu) {
            // Key sense / read: no key available.
            0x00 | 0x01 => cpu.set_reg16(Reg16::Ax, 0),
            _ => {}
        }
    }

    /// PC-98 timer BIOS (INT 1Ch) — the one-shot callback, and cancelling it.
    ///
    /// `AH=02` arms a far routine at `ES:BX` to be called once `CX` BIOS timer
    /// ticks have passed; `AH=01` cancels a pending one. Packen Software's NL /
    /// MUAPLAY / NAX drivers arm it with `CX=2` and then count iterations of a
    /// tight loop until the routine fires, turning the result into the constant
    /// their I/O busy-waits are sized from. With the call unserviced the routine
    /// never fires and the driver counts forever — it never returns to DOS, let
    /// alone goes resident.
    ///
    /// Only the delay's *order of magnitude* reaches the music: the constant
    /// sizes busy-waits, while tempo comes off the OPN timer or the PIT. Other
    /// `AH` values stay in the unimplemented tally rather than being guessed at.
    fn int1c(&mut self, cpu: &mut dyn X86Cpu) {
        match ah(cpu) {
            0x02 => {
                self.bios_timer = Some(BiosTimerReq::Arm {
                    seg: cpu.reg16(Reg16::Es),
                    off: cpu.reg16(Reg16::Bx),
                    ticks: cpu.reg16(Reg16::Cx),
                });
                self.enable_irqs_on_return(cpu);
            }
            0x01 => self.bios_timer = Some(BiosTimerReq::Cancel),
            f => *self.unimpl.entry((0x1C, f)).or_default() += 1,
        }
    }

    fn int21(&mut self, cpu: &mut dyn X86Cpu) -> Option<ExecResult> {
        let f = ah(cpu);
        match f {
            0x00 => return Some(ExecResult::Terminated(0)),
            0x4C => return Some(ExecResult::Terminated(al(cpu))),
            0x31 => {
                // TSR keep-process: shrink the PSP block to DX paragraphs.
                let keep = cpu.reg16(Reg16::Dx);
                let psp = self.psp_seg;
                let _ = self.resize(cpu.mem(), psp, keep.max(0x10));
                return Some(ExecResult::Resident);
            }
            // ---- console output --------------------------------------
            0x02 | 0x06 => {
                let dl = cpu.reg16(Reg16::Dx) as u8;
                if f == 0x06 && dl == 0xFF {
                    // direct console input
                    let b = self.conin.pop_front().unwrap_or(0);
                    set_al(cpu, b);
                    let mut fl = cpu.reg16(Reg16::Flags);
                    if b == 0 {
                        fl |= hoot_cpu::flag::ZF;
                    } else {
                        fl &= !hoot_cpu::flag::ZF;
                    }
                    cpu.set_reg16(Reg16::Flags, fl);
                } else {
                    self.con_out.push(dl);
                    set_al(cpu, dl);
                }
            }
            0x09 => {
                // print '$'-terminated string at DS:DX
                let seg = cpu.reg16(Reg16::Ds);
                let off = cpu.reg16(Reg16::Dx);
                let mut a = lin(seg, off);
                let mem = cpu.mem_ref();
                let mut n = 0;
                while mem[a] != b'$' && n < 0x4000 {
                    self.con_out.push(mem[a]);
                    a += 1;
                    n += 1;
                }
                set_al(cpu, 0x24);
            }
            0x01 | 0x07 | 0x08 => {
                let b = self.conin.pop_front().unwrap_or(0);
                set_al(cpu, b);
            }
            0x0B => {
                // check input status: AL=FF if a char is ready, else 0.
                set_al(cpu, if self.conin.is_empty() { 0x00 } else { 0xFF });
            }
            // ---- DTA --------------------------------------------------
            0x1A => {
                self.dta = (cpu.reg16(Reg16::Ds), cpu.reg16(Reg16::Dx));
            }
            0x2F => {
                cpu.set_reg16(Reg16::Es, self.dta.0);
                cpu.set_reg16(Reg16::Bx, self.dta.1);
            }
            // ---- interrupt vectors -----------------------------------
            0x25 => {
                let v = al(cpu);
                let seg = cpu.reg16(Reg16::Ds);
                let off = cpu.reg16(Reg16::Dx);
                wr16(cpu.mem(), v as usize * 4, off);
                wr16(cpu.mem(), v as usize * 4 + 2, seg);
                self.installed_vectors.insert(v, (seg, off));
            }
            0x35 => {
                let v = al(cpu);
                let off = rd16(cpu.mem_ref(), v as usize * 4);
                let seg = rd16(cpu.mem_ref(), v as usize * 4 + 2);
                cpu.set_reg16(Reg16::Es, seg);
                cpu.set_reg16(Reg16::Bx, off);
            }
            // ---- DOS info --------------------------------------------
            0x30 => {
                // DOS 5.0
                cpu.set_reg16(Reg16::Ax, 0x0005);
                cpu.set_reg16(Reg16::Bx, 0);
                cpu.set_reg16(Reg16::Cx, 0);
            }
            0x19 => set_al(cpu, 0x02), // current drive = C:
            0x2A => {
                // get date: return a fixed date, day-of-week 0
                cpu.set_reg16(Reg16::Cx, 1996);
                cpu.set_reg16(Reg16::Dx, 0x0C18);
                set_al(cpu, 0);
            }
            0x2C => {
                cpu.set_reg16(Reg16::Cx, 0);
                cpu.set_reg16(Reg16::Dx, 0);
            }
            0x33 => set_al(cpu, 0), // ctrl-break state off
            0x50 => self.psp_seg = cpu.reg16(Reg16::Bx),
            0x51 | 0x62 => cpu.set_reg16(Reg16::Bx, self.psp_seg),
            0x52 => {
                // Get List of Lists (DOS internal SysVars pointer).
                cpu.set_reg16(Reg16::Es, SYSVARS_SEG);
                cpu.set_reg16(Reg16::Bx, LOL_OFF);
            }
            // ---- files -----------------------------------------------
            0x3D => {
                let name = self.read_cstr(cpu, cpu.reg16(Reg16::Ds), cpu.reg16(Reg16::Dx));
                if self.find_file(&name).is_some() {
                    let h = self.next_handle;
                    self.next_handle += 1;
                    self.handles.insert(h, OpenFile { name: name.to_uppercase(), pos: 0 });
                    cpu.set_reg16(Reg16::Ax, h);
                    set_cf(cpu, false);
                } else {
                    cpu.set_reg16(Reg16::Ax, 0x0002); // file not found
                    set_cf(cpu, true);
                }
            }
            0x3E => {
                let h = cpu.reg16(Reg16::Bx);
                self.handles.remove(&h);
                set_cf(cpu, false);
            }
            0x3F => {
                let h = cpu.reg16(Reg16::Bx);
                let count = cpu.reg16(Reg16::Cx) as usize;
                let dst = lin(cpu.reg16(Reg16::Ds), cpu.reg16(Reg16::Dx));
                let (data, start) = match self.handles.get(&h) {
                    Some(of) => match self.files.get(&of.name) {
                        Some(d) => (d.clone(), of.pos),
                        None => (Vec::new(), 0),
                    },
                    None => (Vec::new(), 0),
                };
                let n = count.min(data.len().saturating_sub(start));
                cpu.mem()[dst..dst + n].copy_from_slice(&data[start..start + n]);
                if let Some(of) = self.handles.get_mut(&h) {
                    of.pos += n;
                }
                self.read_log.push((h, n));
                cpu.set_reg16(Reg16::Ax, n as u16);
                set_cf(cpu, false);
            }
            0x40 => {
                // write: only stdout/stderr matter for us (console).
                let h = cpu.reg16(Reg16::Bx);
                let count = cpu.reg16(Reg16::Cx) as usize;
                let src = lin(cpu.reg16(Reg16::Ds), cpu.reg16(Reg16::Dx));
                if h == 1 || h == 2 {
                    let mem = cpu.mem_ref();
                    self.con_out.extend_from_slice(&mem[src..src + count]);
                }
                cpu.set_reg16(Reg16::Ax, count as u16);
                set_cf(cpu, false);
            }
            0x42 => {
                // lseek
                let h = cpu.reg16(Reg16::Bx);
                let whence = al(cpu);
                let off = ((cpu.reg16(Reg16::Cx) as u32) << 16) | cpu.reg16(Reg16::Dx) as u32;
                let len = self
                    .handles
                    .get(&h)
                    .and_then(|of| self.files.get(&of.name))
                    .map(|d| d.len() as u32)
                    .unwrap_or(0);
                if let Some(of) = self.handles.get_mut(&h) {
                    let base = match whence {
                        0 => 0,
                        1 => of.pos as u32,
                        _ => len,
                    };
                    let np = base.wrapping_add(off).min(len);
                    of.pos = np as usize;
                    cpu.set_reg16(Reg16::Ax, np as u16);
                    cpu.set_reg16(Reg16::Dx, (np >> 16) as u16);
                }
                set_cf(cpu, false);
            }
            0x44 => {
                // IOCTL get device info: report a plain file (not a device).
                if al(cpu) == 0 {
                    cpu.set_reg16(Reg16::Dx, 0);
                }
                set_cf(cpu, false);
            }
            // ---- memory ----------------------------------------------
            0x48 => {
                let paras = cpu.reg16(Reg16::Bx);
                match self.alloc(cpu.mem(), paras, self.psp_seg) {
                    Some(seg) => {
                        cpu.set_reg16(Reg16::Ax, seg);
                        set_cf(cpu, false);
                    }
                    None => {
                        // Real DOS returns BX = largest available block, so a caller
                        // can retry with a satisfiable size — the ubiquitous "alloc
                        // 0xFFFF, re-alloc the returned BX" memory-probe idiom (used
                        // by e.g. MLALF/ANNEX at 0x234; AH=4A already does this).
                        let max = self.max_free_block(cpu.mem_ref());
                        cpu.set_reg16(Reg16::Ax, 0x0008); // insufficient memory
                        cpu.set_reg16(Reg16::Bx, max);
                        set_cf(cpu, true);
                    }
                }
            }
            0x49 => {
                let seg = cpu.reg16(Reg16::Es);
                self.free(cpu.mem(), seg);
                set_cf(cpu, false);
            }
            0x4A => {
                let seg = cpu.reg16(Reg16::Es);
                let paras = cpu.reg16(Reg16::Bx);
                match self.resize(cpu.mem(), seg, paras) {
                    Ok(()) => set_cf(cpu, false),
                    Err(max) => {
                        cpu.set_reg16(Reg16::Ax, 0x0008);
                        cpu.set_reg16(Reg16::Bx, max);
                        set_cf(cpu, true);
                    }
                }
            }
            // EXEC. Only AL=03 (load overlay) is implemented — it needs no child
            // PSP/pump; the caller invokes the loaded entry itself. AL=00/01 (spawn
            // a child) would need a nested PSP + re-entrant pump; no target needs it.
            0x4B => {
                let subfn = al(cpu);
                let name = self.read_cstr(cpu, cpu.reg16(Reg16::Ds), cpu.reg16(Reg16::Dx));
                let pb = lin(cpu.reg16(Reg16::Es), cpu.reg16(Reg16::Bx));
                if subfn == 0x03 {
                    let load_seg = rd16(cpu.mem_ref(), pb); // param word[0]
                    let reloc = rd16(cpu.mem_ref(), pb + 2); // param word[2]
                    match self.find_file(&name).cloned() {
                        Some(image) => {
                            self.load_overlay(cpu, &image, load_seg, reloc);
                            set_cf(cpu, false);
                        }
                        None => {
                            cpu.set_reg16(Reg16::Ax, 0x0002); // file not found
                            set_cf(cpu, true);
                        }
                    }
                } else {
                    *self.unimpl.entry((0x21, 0x4B)).or_default() += 1;
                    cpu.set_reg16(Reg16::Ax, 0x0001);
                    set_cf(cpu, true);
                }
            }
            _ => {
                *self.unimpl.entry((0x21, f)).or_default() += 1;
                set_cf(cpu, false);
            }
        }
        None
    }

    /// Return from a serviced software interrupt: pop IP/CS/FLAGS off the guest
    /// stack (as `IRET` would) but keep the CF/ZF our handler set as the DOS
    /// return status, restoring all other flags from the caller's saved image.
    /// Set IF in the interrupt frame `iret_return` will restore, so a serviced
    /// BIOS call can hand control back with interrupts enabled. Arming a timed
    /// callback and returning with them still disabled would deadlock the
    /// caller, which is why the real BIOS enables them here.
    pub fn enable_irqs_on_return(&self, cpu: &mut dyn X86Cpu) {
        let base = lin(cpu.reg16(Reg16::Ss), cpu.reg16(Reg16::Sp)) + 4;
        let flags = rd16(cpu.mem_ref(), base) | hoot_cpu::flag::IF;
        wr16(cpu.mem(), base, flags);
    }

    pub fn iret_return(&self, cpu: &mut dyn X86Cpu) {
        let ss = cpu.reg16(Reg16::Ss);
        let sp = cpu.reg16(Reg16::Sp);
        let base = lin(ss, sp);
        let (ret_ip, ret_cs, saved_flags) = {
            let mem = cpu.mem_ref();
            (rd16(mem, base), rd16(mem, base + 2), rd16(mem, base + 4))
        };
        let live = cpu.reg16(Reg16::Flags);
        let status = hoot_cpu::flag::CF | hoot_cpu::flag::ZF;
        let ret_flags = (saved_flags & !status) | (live & status);
        cpu.set_reg16(Reg16::Sp, sp.wrapping_add(6));
        cpu.set_cs_ip(ret_cs, ret_ip);
        cpu.set_reg16(Reg16::Flags, ret_flags);
    }

    /// Read an ASCIIZ string from guest memory (for DOS filename arguments).
    fn read_cstr(&self, cpu: &dyn X86Cpu, seg: u16, off: u16) -> String {
        let mem = cpu.mem_ref();
        let mut a = lin(seg, off);
        let mut s = Vec::new();
        while mem[a] != 0 && s.len() < 128 {
            s.push(mem[a]);
            a += 1;
        }
        String::from_utf8_lossy(&s).into_owned()
    }
}

impl Default for MiniDos {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProgKind {
    Com,
    Exe,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoot_cpu::mock::MockCpu;

    #[test]
    fn arena_alloc_split_free_coalesce() {
        let dos = MiniDos::new();
        let mut cpu = MockCpu::new();
        dos.init_arena(cpu.mem());

        let a = dos.alloc(cpu.mem(), 0x100, 0x1234).unwrap();
        let b = dos.alloc(cpu.mem(), 0x080, 0x5678).unwrap();
        assert!(b > a);
        // Owners recorded in the MCBs.
        assert_eq!(rd16(cpu.mem_ref(), lin(a - 1, 0) + 1), 0x1234);
        assert_eq!(rd16(cpu.mem_ref(), lin(b - 1, 0) + 1), 0x5678);

        // Free the first, then a big alloc should reuse coalesced space.
        dos.free(cpu.mem(), a);
        let c = dos.alloc(cpu.mem(), 0x080, 0x9999).unwrap();
        assert_eq!(c, a); // first-fit lands back in the freed block
    }

    #[test]
    fn resize_shrink_then_grow_back() {
        let dos = MiniDos::new();
        let mut cpu = MockCpu::new();
        dos.init_arena(cpu.mem());
        let a = dos.alloc(cpu.mem(), 0x400, 0x1000).unwrap();
        // Shrink to 0x40 paras (PMD's startup pattern).
        dos.resize(cpu.mem(), a, 0x40).unwrap();
        assert_eq!(rd16(cpu.mem_ref(), lin(a - 1, 0) + 3), 0x40);
        // Grow back into the freed tail.
        dos.resize(cpu.mem(), a, 0x400).unwrap();
        assert_eq!(rd16(cpu.mem_ref(), lin(a - 1, 0) + 3), 0x400);
    }

    #[test]
    fn com_loader_sets_psp_and_entry() {
        let dos_files = |d: &mut MiniDos| d.add_file("PMD_98.COM", vec![0x90, 0xC3]);
        let mut dos = MiniDos::new();
        dos_files(&mut dos);
        let mut cpu = MockCpu::new();
        dos.init_arena(cpu.mem());
        dos.install_trampolines(cpu.mem());

        let (img, kind) = dos.resolve_program("pmd_98").unwrap();
        assert_eq!(kind, ProgKind::Com);
        let psp = dos.load_com(&mut cpu, "pmd_98", &img, b"/K /M8").unwrap();

        // CS:IP at PSP:0x100, all segregs = PSP.
        assert_eq!(cpu.reg16(Reg16::Cs), psp);
        assert_eq!(cpu.reg16(Reg16::Ip), 0x100);
        assert_eq!(cpu.reg16(Reg16::Ss), psp);
        assert_eq!(cpu.reg16(Reg16::Ds), psp);
        // Image bytes landed at PSP:0x100.
        assert_eq!(cpu.mem_ref()[lin(psp, 0x100)], 0x90);
        assert_eq!(cpu.mem_ref()[lin(psp, 0x101)], 0xC3);
        // Command tail: length, text, CR at PSP:0x80.
        assert_eq!(cpu.mem_ref()[lin(psp, 0x80)], 6);
        assert_eq!(&cpu.mem_ref()[lin(psp, 0x81)..lin(psp, 0x87)], b"/K /M8");
        assert_eq!(cpu.mem_ref()[lin(psp, 0x87)], 0x0D); // CR terminator
    }

    #[test]
    fn com_loader_builds_a_real_environment_block() {
        let mut dos = MiniDos::new();
        dos.add_file("OPNDRV.COM", vec![0x90, 0xC3]);
        let mut cpu = MockCpu::new();
        dos.init_arena(cpu.mem());
        dos.install_trampolines(cpu.mem());

        let (img, _) = dos.resolve_program("opndrv").unwrap();
        let psp = dos.load_com(&mut cpu, "opndrv", &img, b"").unwrap();

        // PSP:0x2C names a block below the program, owned by the PSP so the
        // program can free it once it has copied what it wants out.
        let env = rd16(cpu.mem_ref(), lin(psp, 0x2C));
        assert!(env != 0 && env < psp, "env {env:#x} should sit below psp {psp:#x}");
        assert_eq!(rd16(cpu.mem_ref(), lin(env - 1, 0) + 1), psp);

        // Layout: variable strings, the NUL that closes the list, a 0x0001
        // count word, then this program's own path. Drivers that relocate their
        // path into the PSP scan for exactly the double NUL.
        let size = rd16(cpu.mem_ref(), lin(env - 1, 0) + 3) as usize * 16;
        let block = &cpu.mem_ref()[lin(env, 0)..lin(env, 0) + size];
        let end = block.windows(2).position(|w| w == [0, 0]).expect("list terminator");
        assert_eq!(&block[..end], b"COMSPEC=C:\\COMMAND.COM");
        assert_eq!(&block[end + 2..end + 4], &[0x01, 0x00]);
        let path = &block[end + 4..];
        let path = &path[..path.iter().position(|&b| b == 0).unwrap()];
        assert_eq!(path, b"C:\\OPNDRV.COM");
    }

    #[test]
    fn timer_bios_arms_a_callback_and_re_enables_interrupts() {
        let mut dos = MiniDos::new();
        let mut cpu = MockCpu::new();
        dos.init_arena(cpu.mem());
        dos.install_trampolines(cpu.mem());
        cpu.set_ss_sp(0x0900, 0x0100);
        cpu.set_cs_ip(0x1234, 0x0056);

        // The guest arms a one-shot: AH=02, ES:BX = routine, CX = ticks. It does
        // so with interrupts disabled, which is why the BIOS must turn them back
        // on — otherwise the callback it just armed could never be delivered.
        cpu.set_reg16(Reg16::Ax, 0x0200);
        cpu.set_reg16(Reg16::Es, 0x1005);
        cpu.set_reg16(Reg16::Bx, 0x184D);
        cpu.set_reg16(Reg16::Cx, 2);
        cpu.interrupt(0x1C);
        assert!(dos.service_int(&mut cpu, 0x1C).is_none());
        assert_eq!(dos.bios_timer, Some(BiosTimerReq::Arm { seg: 0x1005, off: 0x184D, ticks: 2 }));

        dos.iret_return(&mut cpu);
        assert_ne!(cpu.reg16(Reg16::Flags) & hoot_cpu::flag::IF, 0);
        assert_eq!(cpu.reg16(Reg16::Cs), 0x1234);
        assert_eq!(cpu.reg16(Reg16::Ip), 0x0056);

        // AH=01 cancels. It has to be posted as a request, not a clear: the
        // harness drains this field every step, so by now the arm is long gone
        // from here and only the engine's copy can still fire.
        dos.bios_timer = None; // as the harness leaves it after picking the arm up
        cpu.set_reg16(Reg16::Ax, 0x0100);
        cpu.interrupt(0x1C);
        assert!(dos.service_int(&mut cpu, 0x1C).is_none());
        assert_eq!(dos.bios_timer, Some(BiosTimerReq::Cancel));
    }

    #[test]
    fn exe_loader_applies_relocations() {
        // Minimal MZ: header 2 paras (32 bytes), one reloc pointing at the first
        // word of the load module, one code word to be fixed up.
        let mut img = vec![0u8; 32 + 16];
        img[0] = b'M';
        img[1] = b'Z';
        wr16(&mut img, 0x02, 48 % 512); // bytes in last page
        wr16(&mut img, 0x04, 1); // pages (1*512, trimmed by last-page)
        wr16(&mut img, 0x06, 1); // nreloc
        wr16(&mut img, 0x08, 2); // header paras (32 bytes)
        wr16(&mut img, 0x0A, 0x10); // min alloc paras
        wr16(&mut img, 0x0E, 0x0000); // ss
        wr16(&mut img, 0x10, 0x0100); // sp
        wr16(&mut img, 0x14, 0x0000); // ip
        wr16(&mut img, 0x16, 0x0000); // cs
        wr16(&mut img, 0x18, 0x001C); // reloc table offset
        // reloc entry -> load module offset 0, seg 0
        wr16(&mut img, 0x1C, 0x0000);
        wr16(&mut img, 0x1E, 0x0000);
        // load module: a word = 0x0000 that should become load_seg.
        wr16(&mut img, 0x20, 0x0000);

        let mut dos = MiniDos::new();
        let mut cpu = MockCpu::new();
        dos.init_arena(cpu.mem());
        let psp = dos.load_exe(&mut cpu, "test", &img, b"").unwrap();
        let load_seg = psp + 0x10;
        // CS = load_seg + init_cs(0); relocated word == load_seg.
        assert_eq!(cpu.reg16(Reg16::Cs), load_seg);
        assert_eq!(rd16(cpu.mem_ref(), lin(load_seg, 0)), load_seg);
    }

    struct NullIo;
    impl hoot_cpu::IoBus for NullIo {
        fn out8(&mut self, _port: u16, _val: u8) {}
        fn in8(&mut self, _port: u16) -> u8 {
            0xFF
        }
    }

    // The only test that drives the real (singleton) NP2 core in this crate.
    #[test]
    fn exec_com_prints_string_and_terminates() {
        use hoot_cpu::np2::Np2Cpu;
        let mut cpu = Np2Cpu::new();
        cpu.set_adrsmask(0x000F_FFFF);
        let mut dos = MiniDos::new();
        dos.init_arena(cpu.mem());
        dos.install_trampolines(cpu.mem());

        // COM (loads at PSP:0x100):
        //   B4 09        mov ah,0x09
        //   BA 0C 01     mov dx,0x010C     ; -> "HI$" at image off 0x0C
        //   CD 21        int 0x21          ; print "HI"
        //   B8 00 4C     mov ax,0x4C00
        //   CD 21        int 0x21          ; terminate(0)
        //   "HI$"
        let prog = [
            0xB4, 0x09, 0xBA, 0x0C, 0x01, 0xCD, 0x21, 0xB8, 0x00, 0x4C, 0xCD, 0x21, b'H', b'I',
            b'$',
        ];
        dos.add_file("HELLO.COM", prog.to_vec());

        let mut io = NullIo;
        let res = dos.exec(&mut cpu, &mut io, "HELLO").unwrap();
        assert_eq!(res, ExecResult::Terminated(0));
        assert_eq!(dos.con_out, b"HI");
    }

    #[test]
    fn trampolines_map_back_to_vectors() {
        let dos = MiniDos::new();
        let mut cpu = MockCpu::new();
        dos.install_trampolines(cpu.mem());
        // IVT[0x21] -> TRAMP_SEG:0x42
        assert_eq!(rd16(cpu.mem_ref(), 0x21 * 4), 0x21 * 2);
        assert_eq!(rd16(cpu.mem_ref(), 0x21 * 4 + 2), TRAMP_SEG);
        // The byte there is HLT, and it maps back to vector 0x21.
        assert_eq!(cpu.mem_ref()[lin(TRAMP_SEG, 0x21 * 2)], HLT);
        assert_eq!(dos.trap_vector(TRAMP_SEG, 0x21 * 2), Some(0x21));
        assert_eq!(dos.trap_vector(0x1000, 0), None);
    }
}
