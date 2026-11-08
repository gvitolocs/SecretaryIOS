//! Kindle backend: `/dev/fb0` + mxcfb EPDC updates and an evdev touch reader.
//!
//! **Compiled and type-checked only. It has never run on a Kindle.** Every ABI constant below was
//! re-declared by hand (no code copied) from these sources:
//!
//! * `struct mxcfb_rect`, `struct mxcfb_update_data`, `struct mxcfb_alt_buffer_data`,
//!   `struct mxcfb_update_marker_data`, `MXCFB_SEND_UPDATE` (`_IOW('F', 0x2E, …)`),
//!   `MXCFB_WAIT_FOR_UPDATE_COMPLETE` (`_IOWR('F', 0x2F, …)`), the `WAVEFORM_MODE_*`, `UPDATE_MODE_*` and
//!   `TEMP_USE_AMBIENT` values: the Freescale `include/linux/mxcfb.h` as modified by Lab126 in Amazon's
//!   Kindle 5.x kernel source releases (Kindle "source code notices" page). The Lab126 variant adds
//!   `hist_bw_waveform_mode`/`hist_gray_waveform_mode`, making the struct 72 bytes; Kindle 5.0 used a
//!   64-byte struct and the later "Zelda"/"Rex" boards (Oasis 2+, PW4+) use other layouts, so this module
//!   targets the i.MX6SL boards (PW2/PW3/KT2/KV/KT3) only. Cross-checked against FBInk's
//!   `eink/mxcfb-kindle.h` and KOReader's `ffi/mxcfb_kindle_h.lua` (read for reference only; both are
//!   (A)GPL and nothing was copied).
//! * `FBIOGET_VSCREENINFO`/`FBIOGET_FSCREENINFO` and `struct fb_var_screeninfo`/`fb_fix_screeninfo`:
//!   Linux `include/uapi/linux/fb.h`.
//! * `struct input_event`, `EVIOCGABS`, `EVIOCGRAB`, `EVIOCGNAME`, `EVIOCSCLOCKID`: Linux
//!   `include/uapi/linux/input.h`. `input_event` is declared with `c_ulong` seconds/microseconds, which
//!   is the kernel's layout on 32-bit ARM regardless of musl's 64-bit `time_t`.
//! * ioctl number encoding: Linux `include/uapi/asm-generic/ioctl.h` (ARM uses the generic layout).
use crate::display::{check_regions, Display, FrameMut, RefreshPolicy, UpdateRecord, WaveformMode};
use crate::evdev::{MtDecoder, TouchTransform};
use crate::geom::{Rect, Size};
use crate::input::{Gesture, GestureConfig, GestureRecognizer, Input, PowerEvent, TouchEvent};
use crate::{Error, Result};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

/// `_IOC(dir, type, nr, size)` from asm-generic/ioctl.h.
pub const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr as u32
}

fn ioctl<T>(fd: &impl AsRawFd, req: u32, arg: *mut T) -> io::Result<()> {
    // SAFETY: callers pass a request number whose argument type is `T`, and `arg` points to a valid `T`.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), req as _, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// mxcfb ABI for the Kindle 5.x i.MX6SL kernels.
pub mod mxcfb {
    use super::{ioc, IOC_READ, IOC_WRITE};

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct MxcfbRect {
        pub top: u32,
        pub left: u32,
        pub width: u32,
        pub height: u32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct MxcfbAltBufferData {
        pub phys_addr: u32,
        pub width: u32,
        pub height: u32,
        pub alt_update_region: MxcfbRect,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct MxcfbUpdateData {
        pub update_region: MxcfbRect,
        pub waveform_mode: u32,
        pub update_mode: u32,
        pub update_marker: u32,
        /// Lab126 addition: B/W waveform used by the driver's histogram analysis.
        pub hist_bw_waveform_mode: u32,
        /// Lab126 addition: gray waveform used by the driver's histogram analysis.
        pub hist_gray_waveform_mode: u32,
        pub temp: i32,
        pub flags: u32,
        pub alt_buffer_data: MxcfbAltBufferData,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct MxcfbUpdateMarkerData {
        pub update_marker: u32,
        pub collision_test: u32,
    }

    pub const UPDATE_MODE_PARTIAL: u32 = 0x0;
    pub const UPDATE_MODE_FULL: u32 = 0x1;

    pub const WAVEFORM_MODE_INIT: u32 = 0x0;
    pub const WAVEFORM_MODE_DU: u32 = 0x1;
    pub const WAVEFORM_MODE_GC16: u32 = 0x2;
    pub const WAVEFORM_MODE_GC16_FAST: u32 = 0x3;
    pub const WAVEFORM_MODE_A2: u32 = 0x4;
    pub const WAVEFORM_MODE_GL16: u32 = 0x5;
    pub const WAVEFORM_MODE_GL16_FAST: u32 = 0x6;
    pub const WAVEFORM_MODE_DU4: u32 = 0x7;
    pub const WAVEFORM_MODE_REAGL: u32 = 0x8;
    pub const WAVEFORM_MODE_REAGLD: u32 = 0x9;
    pub const WAVEFORM_MODE_AUTO: u32 = 257;

    pub const TEMP_USE_AMBIENT: i32 = 0x1000;

    pub const MXCFB_SEND_UPDATE: u32 = ioc(
        IOC_WRITE,
        b'F',
        0x2E,
        core::mem::size_of::<MxcfbUpdateData>(),
    );
    pub const MXCFB_WAIT_FOR_UPDATE_COMPLETE: u32 = ioc(
        IOC_READ | IOC_WRITE,
        b'F',
        0x2F,
        core::mem::size_of::<MxcfbUpdateMarkerData>(),
    );

    // Layout checks: all fields are 32-bit, so the sizes are the same on ARMv7 and x86_64.
    const _: () = assert!(core::mem::size_of::<MxcfbRect>() == 16);
    const _: () = assert!(core::mem::size_of::<MxcfbUpdateData>() == 72);
    const _: () = assert!(MXCFB_SEND_UPDATE == 0x4048_462e);
    const _: () = assert!(MXCFB_WAIT_FOR_UPDATE_COMPLETE == 0xc008_462f);

    pub fn waveform(mode: crate::WaveformMode) -> u32 {
        match mode {
            crate::WaveformMode::Du => WAVEFORM_MODE_DU,
            crate::WaveformMode::Gc16 => WAVEFORM_MODE_GC16,
            crate::WaveformMode::Gl16 => WAVEFORM_MODE_GL16,
            crate::WaveformMode::A2 => WAVEFORM_MODE_A2,
        }
    }
}

/// Linux framebuffer ABI (`include/uapi/linux/fb.h`).
pub mod fb {
    use std::os::raw::c_ulong;

    pub const FBIOGET_VSCREENINFO: u32 = 0x4600;
    pub const FBIOGET_FSCREENINFO: u32 = 0x4602;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct FbBitfield {
        pub offset: u32,
        pub length: u32,
        pub msb_right: u32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct FbVarScreeninfo {
        pub xres: u32,
        pub yres: u32,
        pub xres_virtual: u32,
        pub yres_virtual: u32,
        pub xoffset: u32,
        pub yoffset: u32,
        pub bits_per_pixel: u32,
        pub grayscale: u32,
        pub red: FbBitfield,
        pub green: FbBitfield,
        pub blue: FbBitfield,
        pub transp: FbBitfield,
        pub nonstd: u32,
        pub activate: u32,
        pub height: u32,
        pub width: u32,
        pub accel_flags: u32,
        pub pixclock: u32,
        pub left_margin: u32,
        pub right_margin: u32,
        pub upper_margin: u32,
        pub lower_margin: u32,
        pub hsync_len: u32,
        pub vsync_len: u32,
        pub sync: u32,
        pub vmode: u32,
        pub rotate: u32,
        pub colorspace: u32,
        pub reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct FbFixScreeninfo {
        pub id: [u8; 16],
        pub smem_start: c_ulong,
        pub smem_len: u32,
        pub type_: u32,
        pub type_aux: u32,
        pub visual: u32,
        pub xpanstep: u16,
        pub ypanstep: u16,
        pub ywrapstep: u16,
        pub line_length: u32,
        pub mmio_start: c_ulong,
        pub mmio_len: u32,
        pub accel: u32,
        pub capabilities: u16,
        pub reserved: [u16; 2],
    }

    const _: () = assert!(core::mem::size_of::<FbVarScreeninfo>() == 160);
}

/// Linux input ABI (`include/uapi/linux/input.h`).
pub mod input_abi {
    use super::{ioc, IOC_READ, IOC_WRITE};
    use std::os::raw::{c_int, c_ulong};

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct InputEvent {
        pub sec: c_ulong,
        pub usec: c_ulong,
        pub type_: u16,
        pub code: u16,
        pub value: i32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct InputAbsinfo {
        pub value: i32,
        pub minimum: i32,
        pub maximum: i32,
        pub fuzz: i32,
        pub flat: i32,
        pub resolution: i32,
    }

    pub const fn eviocgabs(abs: u16) -> u32 {
        ioc(
            IOC_READ,
            b'E',
            0x40 + abs as u8,
            core::mem::size_of::<InputAbsinfo>(),
        )
    }
    pub const fn eviocgname(len: usize) -> u32 {
        ioc(IOC_READ, b'E', 0x06, len)
    }
    pub const EVIOCGRAB: u32 = ioc(IOC_WRITE, b'E', 0x90, core::mem::size_of::<c_int>());
    pub const EVIOCSCLOCKID: u32 = ioc(IOC_WRITE, b'E', 0xa0, core::mem::size_of::<c_int>());

    const _: () = assert!(EVIOCGRAB == 0x4004_4590);
    const _: () = assert!(eviocgabs(0x35) == 0x8018_4575);
}

/// Framebuffer geometry as reported by the driver.
#[derive(Debug, Clone)]
pub struct FbInfo {
    pub id: String,
    pub var: fb::FbVarScreeninfo,
    pub fix: fb::FbFixScreeninfo,
}

/// Read-only query of `/dev/fb0` (no mapping, no drawing).
pub fn fb_info(path: &str) -> Result<FbInfo> {
    let f = File::open(path)?;
    query_fb(&f)
}

fn query_fb(f: &File) -> Result<FbInfo> {
    let mut var = fb::FbVarScreeninfo::default();
    let mut fix = fb::FbFixScreeninfo::default();
    ioctl(f, fb::FBIOGET_VSCREENINFO, &mut var)?;
    ioctl(f, fb::FBIOGET_FSCREENINFO, &mut fix)?;
    let id = String::from_utf8_lossy(&fix.id)
        .trim_end_matches('\0')
        .to_owned();
    Ok(FbInfo { id, var, fix })
}

/// The EPDC framebuffer, memory-mapped.
pub struct KindleDisplay {
    file: File,
    map: *mut u8,
    map_len: usize,
    offset: usize,
    stride: usize,
    size: Size,
    pub policy: RefreshPolicy,
    /// Block until the controller reports each update complete (`MXCFB_WAIT_FOR_UPDATE_COMPLETE`).
    pub wait_for_complete: bool,
    marker: u32,
    pub info: FbInfo,
}

impl KindleDisplay {
    /// Open and map the framebuffer. Expects the 8-bit grayscale mode of the PW3 (unverified on device).
    pub fn open(path: &str, policy: RefreshPolicy) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let info = query_fb(&file)?;
        if info.var.bits_per_pixel != 8 {
            return Err(Error::Device(format!(
                "{path}: {} bpp, expected 8-bit grayscale",
                info.var.bits_per_pixel
            )));
        }
        let stride = info.fix.line_length as usize;
        let size = Size {
            w: info.var.xres,
            h: info.var.yres,
        };
        let offset = info.var.yoffset as usize * stride + info.var.xoffset as usize;
        let map_len = info.fix.smem_len as usize;
        if stride < size.w as usize || offset + stride * size.h as usize > map_len {
            return Err(Error::Device(format!(
                "{path}: inconsistent geometry {info:?}"
            )));
        }
        // SAFETY: mapping a framebuffer device of `smem_len` bytes, shared, read/write.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self {
            file,
            map: map.cast(),
            map_len,
            offset,
            stride,
            size,
            policy,
            wait_for_complete: false,
            marker: 0,
            info,
        })
    }

    fn send(&mut self, region: Rect, mode: WaveformMode, full: bool) -> Result<u32> {
        self.marker = self.marker.wrapping_add(1).max(1);
        let mut data = mxcfb::MxcfbUpdateData {
            update_region: mxcfb::MxcfbRect {
                top: region.y,
                left: region.x,
                width: region.w,
                height: region.h,
            },
            waveform_mode: mxcfb::waveform(mode),
            update_mode: if full {
                mxcfb::UPDATE_MODE_FULL
            } else {
                mxcfb::UPDATE_MODE_PARTIAL
            },
            update_marker: self.marker,
            hist_bw_waveform_mode: mxcfb::WAVEFORM_MODE_DU,
            hist_gray_waveform_mode: mxcfb::WAVEFORM_MODE_GC16_FAST,
            temp: mxcfb::TEMP_USE_AMBIENT,
            flags: 0,
            alt_buffer_data: Default::default(),
        };
        ioctl(&self.file, mxcfb::MXCFB_SEND_UPDATE, &mut data)?;
        if self.wait_for_complete {
            let mut m = mxcfb::MxcfbUpdateMarkerData {
                update_marker: self.marker,
                collision_test: 0,
            };
            ioctl(&self.file, mxcfb::MXCFB_WAIT_FOR_UPDATE_COMPLETE, &mut m)?;
        }
        Ok(self.marker)
    }
}

impl Drop for KindleDisplay {
    fn drop(&mut self) {
        // SAFETY: `map`/`map_len` come from the successful mmap in `open`.
        unsafe {
            libc::munmap(self.map.cast(), self.map_len);
        }
    }
}

impl Display for KindleDisplay {
    fn size(&self) -> Size {
        self.size
    }
    fn frame_mut(&mut self) -> FrameMut<'_> {
        let len = self.stride * self.size.h as usize;
        // SAFETY: `open` checked offset + len <= map_len; the borrow of `self` keeps the mapping alive.
        let data = unsafe { std::slice::from_raw_parts_mut(self.map.add(self.offset), len) };
        FrameMut {
            data,
            size: self.size,
            stride: self.stride,
        }
    }
    fn update_regions(
        &mut self,
        regions: &[Rect],
        mode: WaveformMode,
    ) -> Result<Vec<UpdateRecord>> {
        check_regions(regions, self.size)?;
        let d = self.policy.decide(mode);
        let full_rect = self.size.rect();
        let targets: &[Rect] = if d.full {
            std::slice::from_ref(&full_rect)
        } else {
            regions
        };
        let mut out = Vec::with_capacity(targets.len());
        for r in targets {
            let seq = self.send(*r, d.mode, d.full)?;
            out.push(UpdateRecord {
                seq,
                region: *r,
                mode: d.mode,
                full: d.full,
                promoted: d.promoted,
            });
        }
        Ok(out)
    }
    fn full_refresh(&mut self) -> Result<UpdateRecord> {
        self.policy.note_full();
        let region = self.size.rect();
        let seq = self.send(region, WaveformMode::Gc16, true)?;
        Ok(UpdateRecord {
            seq,
            region,
            mode: WaveformMode::Gc16,
            full: true,
            promoted: false,
        })
    }
}

/// Name and absolute-axis ranges of an input device (read-only).
pub fn input_device_info(path: &str) -> Result<(String, Vec<(u16, input_abi::InputAbsinfo)>)> {
    let f = File::open(path)?;
    let mut name = [0u8; 128];
    ioctl(&f, input_abi::eviocgname(name.len()), name.as_mut_ptr())?;
    let name = String::from_utf8_lossy(&name)
        .trim_end_matches('\0')
        .to_owned();
    let mut axes = Vec::new();
    for abs in [
        crate::evdev::ABS_X,
        crate::evdev::ABS_Y,
        crate::evdev::ABS_MT_SLOT,
        crate::evdev::ABS_MT_POSITION_X,
        crate::evdev::ABS_MT_POSITION_Y,
        crate::evdev::ABS_MT_TRACKING_ID,
    ] {
        let mut info = input_abi::InputAbsinfo::default();
        if ioctl(&f, input_abi::eviocgabs(abs), &mut info).is_ok() {
            axes.push((abs, info));
        }
    }
    Ok((name, axes))
}

fn monotonic_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// Touch screen reader (`/dev/input/eventN`).
pub struct EvdevInput {
    file: File,
    decoder: MtDecoder,
    recognizer: GestureRecognizer,
    pending: VecDeque<TouchEvent>,
    grabbed: bool,
}

impl EvdevInput {
    /// Open a touch device. Axis maxima come from `EVIOCGABS`; the orientation flags of the PW3 are not
    /// known yet, so the transform starts unrotated — set [`EvdevInput::set_transform`] after measuring.
    pub fn open(path: &str, panel: Size, config: GestureConfig) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        let max = |abs: u16| {
            let mut info = input_abi::InputAbsinfo::default();
            ioctl(&file, input_abi::eviocgabs(abs), &mut info)
                .ok()
                .map(|_| info.maximum)
                .filter(|m| *m > 0)
        };
        let raw_max_x = max(crate::evdev::ABS_MT_POSITION_X)
            .or_else(|| max(crate::evdev::ABS_X))
            .unwrap_or(panel.w as i32 - 1);
        let raw_max_y = max(crate::evdev::ABS_MT_POSITION_Y)
            .or_else(|| max(crate::evdev::ABS_Y))
            .unwrap_or(panel.h as i32 - 1);
        // Timestamps on CLOCK_MONOTONIC so they compare with `monotonic_ms` for long-press ticks.
        let mut clock: libc::c_int = libc::CLOCK_MONOTONIC;
        ioctl(&file, input_abi::EVIOCSCLOCKID, &mut clock)?;
        let transform = TouchTransform {
            raw_max_x,
            raw_max_y,
            panel,
            swap_xy: false,
            mirror_x: false,
            mirror_y: false,
        };
        Ok(Self {
            file,
            decoder: MtDecoder::new(transform),
            recognizer: GestureRecognizer::new(config),
            pending: VecDeque::new(),
            grabbed: false,
        })
    }

    pub fn set_transform(&mut self, t: TouchTransform) {
        self.decoder.transform = t;
    }

    /// Exclusive access (`EVIOCGRAB`): the stock UI stops seeing touches until released or closed.
    pub fn grab(&mut self, on: bool) -> Result<()> {
        // EVIOCGRAB takes its int argument by value.
        // SAFETY: plain integer argument.
        let r = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                input_abi::EVIOCGRAB as _,
                libc::c_int::from(on),
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error().into());
        }
        self.grabbed = on;
        Ok(())
    }

    fn read_available(&mut self) -> Result<()> {
        let mut buf = [input_abi::InputEvent::default(); 64];
        let ev_size = std::mem::size_of::<input_abi::InputEvent>();
        loop {
            // SAFETY: reading plain-old-data structs into a correctly sized buffer.
            let n = unsafe {
                libc::read(
                    self.file.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    ev_size * buf.len(),
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock {
                    return Ok(());
                }
                return Err(e.into());
            }
            let count = n as usize / ev_size;
            let mut out = Vec::new();
            for ev in &buf[..count] {
                // c_ulong is u32 on ARMv7 and u64 on x86_64.
                #[allow(clippy::useless_conversion)]
                let t_ms = u64::from(ev.sec) * 1000 + u64::from(ev.usec) / 1000;
                self.decoder
                    .push(ev.type_, ev.code, ev.value, t_ms, &mut out);
            }
            self.pending.extend(out);
            if count < buf.len() {
                return Ok(());
            }
        }
    }
}

impl Drop for EvdevInput {
    fn drop(&mut self) {
        if self.grabbed {
            let _ = self.grab(false);
        }
    }
}

impl Input for EvdevInput {
    fn next_gesture(&mut self, timeout_ms: Option<u32>) -> Result<Option<Gesture>> {
        let deadline = timeout_ms.map(|t| monotonic_ms() + u64::from(t));
        loop {
            while let Some(ev) = self.pending.pop_front() {
                if let Some(g) = self.recognizer.feed(ev) {
                    return Ok(Some(g));
                }
            }
            let now = monotonic_ms();
            if let Some(g) = self.recognizer.tick(now) {
                return Ok(Some(g));
            }
            if deadline.is_some_and(|d| now >= d) {
                return Ok(None);
            }
            // Sleep until the caller's deadline (the reader's idle deadline) or until a resting finger can
            // become a long-press, whichever is first. With neither, block until the touch screen has data:
            // no periodic wake-ups while the reader is idle.
            let until = match (deadline, self.recognizer.long_press_deadline()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let wait: libc::c_int = until.map_or(-1, |u| {
                u.saturating_sub(now).min(libc::c_int::MAX as u64) as libc::c_int
            });
            let mut pfd = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd.
            let r = unsafe { libc::poll(&mut pfd, 1, wait) };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e.into());
                }
            } else if r > 0 {
                self.read_available()?;
            }
        }
    }
}

/// Power-button / suspend events from the stock framework. **Not implemented: TODO stubs only.**
///
/// On firmware 5.x the power button and suspend/resume are owned by `powerd`, which announces state
/// changes as LIPC events (`lipc-wait-event` on the `com.lab126.powerd` publisher is how community tools
/// observe them). The event names, their order relative to the actual suspend, and whether an app can
/// delay the suspend long enough to draw a GC16 sleep frame have **not** been inspected on this PW3, so no
/// LIPC property or event name is used here as working code. Until they are measured on the device:
///
/// * TODO(device): list the powerd events around a power-button press and an idle suspend
///   (`lipc-wait-event -m com.lab126.powerd '*'`, read-only) and record them in
///   `docs/emulator-validation.md`.
/// * TODO(device): decide whether to subscribe through `liblipc` (dynamically linked, glibc) or a helper
///   process piping `lipc-wait-event` output, given this binary is static musl.
/// * TODO(device): find out how the stock screensaver is suppressed while Lucina Ink owns the screen,
///   without changing the system partition.
pub struct PowerdEvents {
    _private: (),
}

impl PowerdEvents {
    /// Always fails: the real event source is not implemented (see the type docs).
    pub fn open() -> Result<Self> {
        Err(Error::Device(
            "powerd power events are not implemented yet (LIPC events not inspected on the device)"
                .into(),
        ))
    }
    /// Next power event, if any. Unreachable until [`PowerdEvents::open`] is implemented.
    pub fn poll(&mut self) -> Result<Option<PowerEvent>> {
        Ok(None)
    }
}
