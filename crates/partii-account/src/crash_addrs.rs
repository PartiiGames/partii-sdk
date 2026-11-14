//! Raw crash addresses, for symbolizing a stripped build's panic offline.
//!
//! Shipping games are stripped, so `std::backtrace` prints every frame as
//! `<unknown>`. The panic hook (`install_panic_hook`) also records each
//! frame's instruction pointer as an offset from its module's load base,
//! plus each module's build id, and the next flush sends them on the error
//! report (`frames`, `modules`, `build_id`, `main`, `arch`; partii's
//! docs/CRASHES.md "Raw frames" says how to symbolize them). They never
//! change how partii groups the report.
//!
//! Per OS (no new crates; plain FFI to what std already links):
//! - frames: `_Unwind_Backtrace` on Linux, Android, macOS and iOS;
//!   `RtlCaptureStackBackTrace` on Windows.
//! - modules: Linux/Android `dl_iterate_phdr` (the loader's own list, same
//!   bases as /proc/self/maps), build id = the GNU build-id note;
//!   macOS/iOS `dladdr` → the Mach-O header, build id = LC_UUID, plus the
//!   `__TEXT` vmaddr; Windows `GetModuleHandleExW` → the PE image, build id =
//!   the CodeView PDB GUID + age.
//! - offset = ip − module base: an ELF vaddr (Linux), a Windows RVA, or an
//!   offset from the Mach-O header (add the module's `vmaddr`).
//! - Elsewhere (wasm, 32-bit ARM): no frames, and the report has none.
//!
//! Safe inside a panic hook: no panicking paths (checked reads, no slice
//! indexing), bounded work (`MAX_FRAMES`, `MAX_MODULES`), and only module
//! file names (no player home folders).
//!
//! From tankii's `crash_addrs.rs` (MIT, Luzzotica LLC).

use serde_json::{json, Value};

/// Frames kept per capture (a Bevy system's panic is ~40 deep).
pub(crate) const MAX_FRAMES: usize = 96;
/// Distinct modules kept per capture; frames in others say `?`.
pub(crate) const MAX_MODULES: usize = 24;
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Module {
    /// File name only (no folders).
    pub(crate) name: String,
    /// `gnu:<hex>`, `macho:<UUID>` or `pdb:<GUID><age>`; empty if none.
    pub(crate) build_id: String,
    pub(crate) base: usize,
    /// Bytes from `base` that belong to the module (offsets are below it).
    pub(crate) size: u64,
    /// Mach-O only: the `__TEXT` link address (file address = vmaddr + offset).
    pub(crate) vmaddr: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Capture {
    /// (index into `modules`, or None for an ip outside every module; offset
    /// from that module's base, or the raw ip when None).
    pub(crate) frames: Vec<(Option<usize>, u64)>,
    pub(crate) modules: Vec<Module>,
    /// Index of the module holding this code (the game binary).
    pub(crate) main: Option<usize>,
}

impl Capture {
    /// The report's fields: `{frames: [{module, offset}], modules: [{module,
    /// build_id?, size, vmaddr?}], build_id?, main?, arch}`; empty when there
    /// are no frames (an unsupported target).
    pub(crate) fn to_json(&self) -> Value {
        if self.frames.is_empty() {
            return json!({});
        }
        let name = |m: Option<usize>| {
            m.and_then(|i| self.modules.get(i))
                .map(|m| m.name.as_str())
                .unwrap_or("?")
        };
        let frames: Vec<Value> = self
            .frames
            .iter()
            .map(|(m, off)| json!({ "module": name(*m), "offset": format!("{off:#x}") }))
            .collect();
        let modules: Vec<Value> = self
            .modules
            .iter()
            .map(|m| {
                let mut v = json!({ "module": m.name, "size": format!("{:#x}", m.size) });
                if !m.build_id.is_empty() {
                    v["build_id"] = json!(m.build_id);
                }
                if m.vmaddr != 0 {
                    v["vmaddr"] = json!(format!("{:#x}", m.vmaddr));
                }
                v
            })
            .collect();
        let main = self.main.and_then(|i| self.modules.get(i));
        let mut v = json!({ "frames": frames, "modules": modules, "arch": std::env::consts::ARCH });
        if let Some(m) = main {
            v["main"] = json!(m.name);
            if !m.build_id.is_empty() {
                v["build_id"] = json!(m.build_id);
            }
        }
        v
    }
}

/// This thread's stack now, as module + offset.
#[inline(never)]
pub(crate) fn capture() -> Capture {
    let mut ips = [0usize; MAX_FRAMES];
    let n = os::ips(&mut ips).min(MAX_FRAMES);
    let mut cap = Capture {
        frames: Vec::with_capacity(n),
        modules: Vec::with_capacity(8),
        main: None,
    };
    cap.main = locate(&mut cap.modules, capture as *const () as usize).map(|(i, _)| i);
    for &ip in ips.iter().take(n) {
        let f = match locate(&mut cap.modules, ip) {
            Some((i, off)) => (Some(i), off),
            None => (None, ip as u64),
        };
        cap.frames.push(f);
    }
    cap
}

/// The module holding `ip` (index, offset), looked up once per module.
fn locate(modules: &mut Vec<Module>, ip: usize) -> Option<(usize, u64)> {
    for (i, m) in modules.iter().enumerate() {
        if ip >= m.base && ((ip - m.base) as u64) < m.size {
            return Some((i, (ip - m.base) as u64));
        }
    }
    if modules.len() >= MAX_MODULES {
        return None;
    }
    let m = os::module_of(ip)?;
    if ip < m.base || ((ip - m.base) as u64) >= m.size {
        return None;
    }
    let off = (ip - m.base) as u64;
    modules.push(m);
    Some((modules.len() - 1, off))
}

/// Longest module name sent (partii drops longer ones).
const NAME_MAX: usize = 128;

/// The file name of a path, at most `NAME_MAX` characters, no control characters.
#[allow(dead_code)]
fn basename(path: &str) -> String {
    let name: String = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(NAME_MAX)
        .collect();
    if name.is_empty() {
        "?".into()
    } else {
        name
    }
}

/// Hex of a byte run, lower case.
#[allow(dead_code)]
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Unaligned little-endian reads from a loaded image (the caller checks bounds).
#[allow(dead_code)]
unsafe fn rd<T: Copy>(base: usize, off: usize) -> T {
    std::ptr::read_unaligned((base + off) as *const T)
}

// ── Frames: the unwinder std itself uses ───────────────────────────────────

#[cfg(all(unix, not(target_arch = "arm")))]
#[allow(dead_code)]
mod unwind {
    use std::ffi::{c_int, c_void};

    type Trace = unsafe extern "C" fn(ctx: *mut c_void, arg: *mut c_void) -> c_int;
    extern "C" {
        fn _Unwind_Backtrace(trace: Trace, arg: *mut c_void) -> c_int;
        fn _Unwind_GetIP(ctx: *mut c_void) -> usize;
    }
    const CONTINUE: c_int = 0; // _URC_NO_REASON
    const STOP: c_int = 5; // _URC_END_OF_STACK

    struct Buf<'a> {
        out: &'a mut [usize],
        n: usize,
    }

    unsafe extern "C" fn step(ctx: *mut c_void, arg: *mut c_void) -> c_int {
        let b = &mut *(arg as *mut Buf);
        let ip = _Unwind_GetIP(ctx);
        if ip == 0 {
            return STOP;
        }
        match b.out.get_mut(b.n) {
            Some(slot) => {
                *slot = ip;
                b.n += 1;
                CONTINUE
            }
            None => STOP,
        }
    }

    pub fn ips(out: &mut [usize]) -> usize {
        let mut b = Buf { out, n: 0 };
        unsafe { _Unwind_Backtrace(step, &mut b as *mut Buf as *mut c_void) };
        b.n
    }
}

// ── Modules ────────────────────────────────────────────────────────────────

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_pointer_width = "64",
    not(target_arch = "arm")
))]
mod os {
    use super::{basename, hex, rd, Module};
    use std::ffi::{c_char, c_int, c_void, CStr};

    pub use super::unwind::ips;

    #[repr(C)]
    struct Phdr {
        p_type: u32,
        p_flags: u32,
        p_offset: u64,
        p_vaddr: u64,
        p_paddr: u64,
        p_filesz: u64,
        p_memsz: u64,
        p_align: u64,
    }
    #[repr(C)]
    struct Info {
        addr: usize,
        name: *const c_char,
        phdr: *const Phdr,
        phnum: u16,
    }
    extern "C" {
        fn dl_iterate_phdr(
            cb: unsafe extern "C" fn(*mut Info, usize, *mut c_void) -> c_int,
            data: *mut c_void,
        ) -> c_int;
    }
    const PT_LOAD: u32 = 1;
    const PT_NOTE: u32 = 4;
    const NT_GNU_BUILD_ID: u32 = 3;

    struct Find {
        ip: usize,
        hit: bool,
        base: usize,
        size: u64,
        name: *const c_char,
        id: [u8; 64],
        id_len: usize,
    }

    /// The GNU build-id in one PT_NOTE segment, into `out`.
    unsafe fn build_id(start: usize, len: usize, out: &mut [u8; 64]) -> Option<usize> {
        let mut at = 0usize;
        while at + 12 <= len {
            let namesz = rd::<u32>(start, at) as usize;
            let descsz = rd::<u32>(start, at + 4) as usize;
            let ty = rd::<u32>(start, at + 8);
            let name_at = at + 12;
            let desc_at = name_at + ((namesz + 3) & !3);
            let next = desc_at + ((descsz + 3) & !3);
            if next > len || next <= at {
                return None;
            }
            if ty == NT_GNU_BUILD_ID
                && namesz == 4
                && rd::<[u8; 4]>(start, name_at) == *b"GNU\0"
                && descsz <= out.len()
            {
                for (i, slot) in out.iter_mut().take(descsz).enumerate() {
                    *slot = rd::<u8>(start, desc_at + i);
                }
                return Some(descsz);
            }
            at = next;
        }
        None
    }

    unsafe extern "C" fn each(info: *mut Info, _size: usize, data: *mut c_void) -> c_int {
        let f = &mut *(data as *mut Find);
        let info = &*info;
        if info.phdr.is_null() {
            return 0;
        }
        let phdrs = std::slice::from_raw_parts(info.phdr, info.phnum as usize);
        let mut end = 0u64;
        let mut hit = false;
        for p in phdrs.iter().filter(|p| p.p_type == PT_LOAD) {
            let start = info.addr.wrapping_add(p.p_vaddr as usize);
            if f.ip >= start && ((f.ip - start) as u64) < p.p_memsz {
                hit = true;
            }
            end = end.max(p.p_vaddr.saturating_add(p.p_memsz));
        }
        if !hit {
            return 0;
        }
        f.hit = true;
        f.base = info.addr;
        f.size = end;
        f.name = info.name;
        for p in phdrs.iter().filter(|p| p.p_type == PT_NOTE) {
            if let Some(n) = build_id(
                info.addr.wrapping_add(p.p_vaddr as usize),
                p.p_memsz as usize,
                &mut f.id,
            ) {
                f.id_len = n;
                break;
            }
        }
        1
    }

    pub fn module_of(ip: usize) -> Option<Module> {
        let mut f = Find {
            ip,
            hit: false,
            base: 0,
            size: 0,
            name: std::ptr::null(),
            id: [0; 64],
            id_len: 0,
        };
        unsafe { dl_iterate_phdr(each, &mut f as *mut Find as *mut c_void) };
        if !f.hit {
            return None;
        }
        let path = if f.name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(f.name) }
                .to_string_lossy()
                .into_owned()
        };
        // The executable itself is listed with an empty name.
        let name = if path.is_empty() {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "exe".into())
        } else {
            basename(&path)
        };
        let build_id = if f.id_len > 0 {
            format!("gnu:{}", hex(&f.id[..f.id_len.min(64)]))
        } else {
            String::new()
        };
        Some(Module {
            name,
            build_id,
            base: f.base,
            size: f.size,
            vmaddr: 0,
        })
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod os {
    use super::{basename, rd, Module};
    use std::ffi::{c_char, c_int, c_void, CStr};

    pub use super::unwind::ips;

    #[repr(C)]
    struct DlInfo {
        fname: *const c_char,
        fbase: *mut c_void,
        sname: *const c_char,
        saddr: *mut c_void,
    }
    extern "C" {
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    }
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const LC_SEGMENT_64: u32 = 0x19;
    const LC_UUID: u32 = 0x1b;

    pub fn module_of(ip: usize) -> Option<Module> {
        let mut info = DlInfo {
            fname: std::ptr::null(),
            fbase: std::ptr::null_mut(),
            sname: std::ptr::null(),
            saddr: std::ptr::null_mut(),
        };
        if unsafe { dladdr(ip as *const c_void, &mut info) } == 0 || info.fbase.is_null() {
            return None;
        }
        let base = info.fbase as usize;
        let name = if info.fname.is_null() {
            "?".into()
        } else {
            basename(&unsafe { CStr::from_ptr(info.fname) }.to_string_lossy())
        };
        // The Mach-O header: LC_UUID and the segments' extent.
        let (mut uuid, mut text, mut lo, mut hi) = (None::<[u8; 16]>, None::<u64>, u64::MAX, 0u64);
        unsafe {
            if rd::<u32>(base, 0) != MH_MAGIC_64 {
                return None;
            }
            let ncmds = rd::<u32>(base, 16).min(1024) as usize;
            let sizeofcmds = rd::<u32>(base, 20) as usize;
            let mut at = 32usize;
            for _ in 0..ncmds {
                if at + 8 > 32 + sizeofcmds {
                    break;
                }
                let cmd = rd::<u32>(base, at);
                let size = rd::<u32>(base, at + 4) as usize;
                if size < 8 {
                    break;
                }
                if cmd == LC_UUID && size >= 24 {
                    uuid = Some(rd::<[u8; 16]>(base, at + 8));
                } else if cmd == LC_SEGMENT_64 && size >= 48 {
                    let seg = rd::<[u8; 16]>(base, at + 8);
                    let (vmaddr, vmsize) = (rd::<u64>(base, at + 24), rd::<u64>(base, at + 32));
                    if seg.starts_with(b"__TEXT\0") {
                        text = Some(vmaddr);
                    }
                    if !seg.starts_with(b"__PAGEZERO\0") && vmsize > 0 {
                        lo = lo.min(vmaddr);
                        hi = hi.max(vmaddr.saturating_add(vmsize));
                    }
                }
                at += size;
            }
        }
        let vmaddr = text.unwrap_or(lo);
        let size = hi.saturating_sub(vmaddr);
        let build_id = uuid
            .map(|u| {
                let h = super::hex(&u).to_uppercase();
                format!(
                    "macho:{}-{}-{}-{}-{}",
                    &h[0..8],
                    &h[8..12],
                    &h[12..16],
                    &h[16..20],
                    &h[20..32]
                )
            })
            .unwrap_or_default();
        Some(Module {
            name,
            build_id,
            base,
            size,
            vmaddr,
        })
    }
}

#[cfg(windows)]
mod os {
    use super::{basename, rd, Module};
    use std::ffi::c_void;

    type Hmodule = *mut c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn RtlCaptureStackBackTrace(
            skip: u32,
            count: u32,
            out: *mut *mut c_void,
            hash: *mut u32,
        ) -> u16;
        fn GetModuleHandleExW(flags: u32, name: *const u16, out: *mut Hmodule) -> i32;
        fn GetModuleFileNameW(module: Hmodule, buf: *mut u16, len: u32) -> u32;
    }
    const FROM_ADDRESS: u32 = 0x4;
    const UNCHANGED_REFCOUNT: u32 = 0x2;

    pub fn ips(out: &mut [usize]) -> usize {
        let n = unsafe {
            RtlCaptureStackBackTrace(
                0,
                out.len().min(u16::MAX as usize) as u32,
                out.as_mut_ptr() as *mut *mut c_void,
                std::ptr::null_mut(),
            )
        };
        n as usize
    }

    pub fn module_of(ip: usize) -> Option<Module> {
        let mut h: Hmodule = std::ptr::null_mut();
        if unsafe {
            GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, ip as *const u16, &mut h)
        } == 0
            || h.is_null()
        {
            return None;
        }
        let base = h as usize;
        let mut buf = [0u16; 520];
        let n = unsafe { GetModuleFileNameW(h, buf.as_mut_ptr(), buf.len() as u32) } as usize;
        let name = basename(&String::from_utf16_lossy(&buf[..n.min(buf.len())]));
        let (mut size, mut build_id) = (0u64, String::new());
        unsafe {
            if rd::<u16>(base, 0) != 0x5a4d {
                return None;
            }
            let pe = rd::<u32>(base, 0x3c) as usize;
            if pe > 4096 || rd::<u32>(base, pe) != 0x0000_4550 {
                return None;
            }
            let opt = pe + 24;
            let dirs = match rd::<u16>(base, opt) {
                0x20b => opt + 112,
                0x10b => opt + 96,
                _ => return None,
            };
            size = rd::<u32>(base, opt + 56) as u64;
            let count = rd::<u32>(base, dirs - 4);
            if count > 6 {
                let (rva, len) = (
                    rd::<u32>(base, dirs + 6 * 8) as u64,
                    rd::<u32>(base, dirs + 6 * 8 + 4) as u64,
                );
                if rva + len <= size {
                    for i in 0..(len / 28).min(16) {
                        let e = (rva + i * 28) as usize;
                        let (ty, raw) = (rd::<u32>(base, e + 12), rd::<u32>(base, e + 20) as u64);
                        if ty == 2
                            && raw + 24 <= size
                            && rd::<u32>(base, raw as usize) == 0x5344_5352
                        {
                            let r = raw as usize;
                            let (d1, d2, d3) = (
                                rd::<u32>(base, r + 4),
                                rd::<u16>(base, r + 8),
                                rd::<u16>(base, r + 10),
                            );
                            let d4 = rd::<[u8; 8]>(base, r + 12);
                            let age = rd::<u32>(base, r + 20);
                            build_id = format!(
                                "pdb:{d1:08X}{d2:04X}{d3:04X}{}{age:X}",
                                super::hex(&d4).to_uppercase()
                            );
                            break;
                        }
                    }
                }
            }
        }
        Some(Module {
            name,
            build_id,
            base,
            size,
            vmaddr: 0,
        })
    }
}

#[cfg(not(any(
    all(
        any(target_os = "linux", target_os = "android"),
        target_pointer_width = "64",
        not(target_arch = "arm")
    ),
    target_os = "macos",
    target_os = "ios",
    windows
)))]
mod os {
    use super::Module;
    pub fn ips(_out: &mut [usize]) -> usize {
        0
    }
    pub fn module_of(_ip: usize) -> Option<Module> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[inline(never)]
    fn probe() -> Capture {
        let c = capture();
        std::hint::black_box(c)
    }

    /// The capture of the test binary itself: frames land in named modules,
    /// inside their extent, and one is in `probe`.
    #[test]
    fn frames_are_offsets_inside_named_modules() {
        let c = probe();
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            assert!(c.frames.len() >= 3, "{c:?}");
        }
        if c.frames.is_empty() {
            return;
        }
        let main = c.main.expect("this code's module");
        let me = std::env::current_exe().unwrap();
        assert_eq!(
            c.modules[main].name,
            me.file_name().unwrap().to_string_lossy(),
            "{c:?}"
        );
        for m in &c.modules {
            assert!(
                !m.name.is_empty() && !m.name.contains('/') && !m.name.contains('\\'),
                "{m:?}"
            );
            assert!(m.size > 0, "{m:?}");
        }
        for (m, off) in &c.frames {
            if let Some(i) = m {
                assert!(
                    *off < c.modules[*i].size,
                    "offset {off:#x} past {:?}",
                    c.modules[*i]
                );
            }
        }
        assert!(
            c.frames.iter().filter(|(m, _)| *m == Some(main)).count() >= 2,
            "{c:?}"
        );
        if cfg!(not(windows)) {
            // A return address inside probe (Windows debug builds call through thunks).
            let p = probe as *const () as usize - c.modules[main].base;
            assert!(
                c.frames
                    .iter()
                    .any(|(m, off)| *m == Some(main) && *off > p as u64 && *off < p as u64 + 4096),
                "no frame in probe at {p:#x}: {c:?}"
            );
        }
        if cfg!(target_os = "macos") {
            assert!(
                c.modules[main].build_id.starts_with("macho:") && c.modules[main].vmaddr > 0,
                "{:?}",
                c.modules[main]
            );
        }
    }

    #[test]
    fn json_stays_bounded() {
        let mut c = probe();
        // A worst case: every frame and module slot full, long names.
        c.modules.resize(
            MAX_MODULES,
            Module {
                name: "x".repeat(NAME_MAX),
                build_id: format!("gnu:{}", "a".repeat(80)),
                base: 0,
                size: 1 << 40,
                vmaddr: 0x1_0000_0000,
            },
        );
        c.frames
            .resize(MAX_FRAMES, (Some(MAX_MODULES - 1), u64::MAX));
        let j = c.to_json();
        assert_eq!(j["frames"].as_array().unwrap().len(), MAX_FRAMES);
        assert!(
            j["frames"][0]["module"].is_string()
                && j["frames"][0]["offset"].as_str().unwrap().starts_with("0x")
        );
        // partii keeps at most 24 KB of them per sample.
        assert!(j.to_string().len() < 24 * 1024, "{}", j.to_string().len());
    }

    /// The fields have the shapes partii checks (and drops otherwise): file
    /// names, 0x hex, gnu:/macho:/pdb: build ids.
    #[test]
    fn json_has_the_shapes_partii_accepts() {
        let c = probe();
        let j = c.to_json();
        if c.frames.is_empty() {
            assert_eq!(j, json!({}));
            return;
        }
        let hex = |v: &Value| {
            v.as_str().is_some_and(|s| {
                s.len() > 2
                    && s.len() <= 18
                    && s.starts_with("0x")
                    && s[2..].chars().all(|ch| ch.is_ascii_hexdigit())
            })
        };
        let id = |v: &Value| {
            v.as_str().is_some_and(|s| {
                let (scheme, rest) = s.split_once(':').unwrap_or(("", ""));
                ["gnu", "macho", "pdb"].contains(&scheme)
                    && !rest.is_empty()
                    && rest.len() <= 80
                    && rest.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-')
            })
        };
        let name = |v: &Value| {
            v.as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 128 && !s.contains(['/', '\\']))
        };
        for f in j["frames"].as_array().unwrap() {
            assert!(name(&f["module"]) && hex(&f["offset"]), "{f}");
        }
        for m in j["modules"].as_array().unwrap() {
            assert!(name(&m["module"]) && hex(&m["size"]), "{m}");
            assert!(m.get("build_id").is_none_or(id), "{m}");
            assert!(m.get("vmaddr").is_none_or(hex), "{m}");
        }
        assert!(name(&j["main"]), "{j}");
        // Mach-O always has an LC_UUID; an ELF build id depends on the linker.
        if cfg!(target_os = "macos") {
            assert!(id(&j["build_id"]), "{j}");
        }
        assert_eq!(j["arch"], std::env::consts::ARCH);
        assert_eq!(basename("/Users/someone/game/My Game"), "My Game");
        assert_eq!(basename("C:\\Users\\x\\game.exe"), "game.exe");
        assert_eq!(basename(&format!("/a/{}", "y".repeat(300))).len(), NAME_MAX);
        assert_eq!(basename("/a/"), "?");
    }
}
