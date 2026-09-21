//! Windows screen-capture primitives, built directly on Win32.
//!
//! # Why not a capture crate
//!
//! The single hardest requirement in this app is that region selection works
//! correctly across monitors that have *different DPI scaling* — a 150%-scaled
//! laptop panel next to a 100% external display, say. Getting that right depends
//! entirely on knowing which coordinate space every number is in, and most
//! capture crates are vague about it.
//!
//! So this module does one deliberate thing: it treats **virtual-screen
//! coordinates in physical pixels** as the single coordinate space for the whole
//! app, and never leaves it. `BitBlt` from the screen DC reads the desktop that
//! DWM has already composited, which means one call returns a single image whose
//! pixel grid *is* that coordinate space. Cropping a selection that spans two
//! monitors is then plain arithmetic on one buffer — there is no stitching step
//! to get wrong, and no per-monitor scale factor to accidentally apply twice.
//!
//! # DPI awareness
//!
//! All of the above only holds if the process is Per-Monitor-DPI-Aware v2. If it
//! were not, Windows would silently hand back *virtualised* coordinates and a
//! stretched, blurry capture on any scaled display. [`ensure_dpi_awareness`] is
//! called once at startup to guarantee it.
//!
//! # Known limitation
//!
//! GDI reads the composited desktop, so it captures normal application windows,
//! browsers and video playback correctly. It cannot see content drawn through a
//! hardware overlay plane or protected by DRM, which will appear black — the same
//! behaviour as most classic screenshot tools. The module boundary here is drawn
//! so a Windows-Graphics-Capture backend can be added later without any caller
//! changing.

use std::ffi::c_void;
use std::mem::size_of;

use image::RgbaImage;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, HMONITOR,
    MONITORINFO, MONITORINFOEXW, SRCCOPY,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetTopWindow, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, GW_HWNDNEXT,
    WS_EX_TOOLWINDOW,
};

/// Anything that can go wrong grabbing pixels from the OS.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("the requested capture area is empty")]
    EmptyArea,
    #[error("no foreground window could be identified")]
    NoForegroundWindow,
    #[error("Windows GDI call failed: {0}")]
    Gdi(String),
}

/// A connected display, in virtual-screen coordinates (physical pixels).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    /// Stable device path, e.g. `\\.\DISPLAY1`. Used to remember a chosen monitor.
    pub id: String,
    /// Label for the UI, e.g. `Display 1 — 2560 x 1440 (Primary)`.
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// DPI scaling, e.g. `1.5` for a display set to 150%.
    pub scale_factor: f64,
    pub is_primary: bool,
}

/// The bounding box of every display combined. The origin is often negative,
/// because the primary display sits at (0, 0) and others may be to its left or
/// above it.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualDesktop {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl VirtualDesktop {
    pub fn as_rect(self) -> Bounds {
        Bounds {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

/// A rectangle in virtual-screen coordinates (physical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Bounds {
    pub fn right(self) -> i32 {
        self.x + self.width as i32
    }

    pub fn bottom(self) -> i32 {
        self.y + self.height as i32
    }

    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Clip this rectangle to `other`, returning `None` if they do not overlap.
    ///
    /// Used to keep a window that hangs off the edge of the desktop — or a
    /// region drag that ran past it — inside the area we can actually read.
    pub fn intersect(self, other: Bounds) -> Option<Bounds> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        if right <= x || bottom <= y {
            return None;
        }
        Some(Bounds {
            x,
            y,
            width: (right - x) as u32,
            height: (bottom - y) as u32,
        })
    }

    fn from_rect(r: RECT) -> Bounds {
        Bounds {
            x: r.left,
            y: r.top,
            width: (r.right - r.left).max(0) as u32,
            height: (r.bottom - r.top).max(0) as u32,
        }
    }
}

/// Captured pixels plus where they came from on the virtual desktop.
pub struct Frame {
    pub image: RgbaImage,
    /// Virtual-screen coordinate of this frame's top-left pixel. Keeping this
    /// attached is what lets a region crop be done later without re-deriving
    /// which monitor anything was on.
    pub origin: (i32, i32),
}

impl Frame {
    /// Crop to `area`, given in virtual-screen coordinates.
    ///
    /// `area` is clipped to the frame first, so a selection dragged past the
    /// edge of the desktop yields the visible part rather than an error.
    pub fn crop(&self, area: Bounds) -> Result<Frame, CaptureError> {
        let frame_bounds = Bounds {
            x: self.origin.0,
            y: self.origin.1,
            width: self.image.width(),
            height: self.image.height(),
        };

        let clipped = area
            .intersect(frame_bounds)
            .ok_or(CaptureError::EmptyArea)?;

        // Translate from virtual-screen space into this frame's pixel space.
        let local_x = (clipped.x - self.origin.0) as u32;
        let local_y = (clipped.y - self.origin.1) as u32;

        let cropped =
            image::imageops::crop_imm(&self.image, local_x, local_y, clipped.width, clipped.height)
                .to_image();

        Ok(Frame {
            image: cropped,
            origin: (clipped.x, clipped.y),
        })
    }
}

// ---------------------------------------------------------------------------
// GDI handle guards
//
// Screenshot code paths have several early returns, and a leaked device context
// or bitmap is a real resource leak in a process that stays resident in the tray
// all day. These guards make cleanup automatic.
// ---------------------------------------------------------------------------

struct ScreenDc(HDC);

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // Passing None for the window matches the GetDC(None) that produced it.
        unsafe { ReleaseDC(None, self.0) };
    }
}

struct MemDc(HDC);

impl Drop for MemDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Opt the process into Per-Monitor-DPI-Aware v2.
///
/// Must run before any window is created. If a manifest already declared an
/// awareness level the call fails harmlessly and is ignored — the goal is only
/// to guarantee we are never left in the legacy virtualised mode, which would
/// make captures on scaled displays blurry and mis-positioned.
pub fn ensure_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Bounding box of all displays, in virtual-screen coordinates.
pub fn virtual_desktop() -> VirtualDesktop {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    unsafe {
        VirtualDesktop {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(0) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(0) as u32,
        }
    }
}

/// Enumerate connected displays, ordered with the primary display first.
pub fn monitors() -> Vec<MonitorInfo> {
    let mut handles: Vec<HMONITOR> = Vec::new();

    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_proc),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        );
    }

    let mut monitors: Vec<MonitorInfo> = handles.into_iter().filter_map(describe_monitor).collect();

    // Primary first, then left-to-right, top-to-bottom. Stable ordering keeps
    // the "Display 1 / Display 2" labels from shuffling between launches.
    monitors.sort_by(|a, b| {
        b.is_primary
            .cmp(&a.is_primary)
            .then(a.x.cmp(&b.x))
            .then(a.y.cmp(&b.y))
    });

    for (index, monitor) in monitors.iter_mut().enumerate() {
        // Plain ASCII punctuation on purpose: this label is written into the
        // history index and read back by tooling that may not agree about
        // encoding, and a mojibaked em dash is not worth the typography.
        monitor.label = format!(
            "Display {} - {} x {}{}",
            index + 1,
            monitor.width,
            monitor.height,
            if monitor.is_primary { " (Primary)" } else { "" }
        );
    }

    monitors
}

unsafe extern "system" fn enum_monitor_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // Safety: `data` is the &mut Vec we passed to EnumDisplayMonitors, and the
    // callback only runs for the duration of that call.
    let handles = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    handles.push(monitor);
    TRUE
}

fn describe_monitor(handle: HMONITOR) -> Option<MonitorInfo> {
    unsafe {
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        // GetMonitorInfoW takes a MONITORINFO*; the EX variant is a superset and
        // is selected by the larger cbSize set above.
        if !GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool()
        {
            return None;
        }

        let rect = info.monitorInfo.rcMonitor;
        let device = String::from_utf16_lossy(&info.szDevice)
            .trim_end_matches('\0')
            .to_string();

        // MONITORINFOF_PRIMARY == 1
        let is_primary = info.monitorInfo.dwFlags & 1 != 0;

        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        // Falls back to 96 (100%) if the call fails, which is the correct
        // assumption for an unscaled display.
        let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);

        Some(MonitorInfo {
            id: device,
            // Replaced with an indexed label by `monitors()` once sorted.
            label: String::new(),
            x: rect.left,
            y: rect.top,
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
            scale_factor: dpi_x as f64 / 96.0,
            is_primary,
        })
    }
}

/// Capture an arbitrary rectangle of the virtual desktop.
///
/// This is the single primitive every capture mode is built from. `area` is in
/// virtual-screen coordinates and is clipped to the desktop before reading.
pub fn capture_area(area: Bounds) -> Result<Frame, CaptureError> {
    let (pixels, area) = grab(area, true)?;
    let image = RgbaImage::from_raw(area.width, area.height, pixels)
        .ok_or_else(|| CaptureError::Gdi("pixel buffer did not match image size".into()))?;
    Ok(Frame {
        image,
        origin: (area.x, area.y),
    })
}

/// A reusable capture context for recording.
///
/// Recording grabs the same rectangle over and over, so everything that does
/// not change between frames is built once. [`grab`] creates a screen DC, a
/// memory DC, a bitmap and an output buffer on *every* call, which is fine for
/// a single screenshot and ruinous thirty times a second.
///
/// It also deliberately omits `CAPTUREBLT`. That flag is what makes layered and
/// transparent windows appear in a still capture, but it forces a far more
/// expensive path through GDI — enough, on a large region, to push a single
/// frame past its slot and make the recorder drop frames on hardware that
/// should have no trouble at all.
///
/// Not `Send`: GDI objects belong to the thread that made them, so a recording
/// builds this on its own thread and keeps it there.
pub struct FrameGrabber {
    screen: ScreenDc,
    mem: MemDc,
    bitmap: Bitmap,
    area: Bounds,
    /// Reused between frames so a recording does not allocate megabytes per frame.
    buffer: Vec<u8>,
}

impl FrameGrabber {
    pub fn new(area: Bounds) -> Result<Self, CaptureError> {
        let desktop = virtual_desktop().as_rect();
        let area = area.intersect(desktop).ok_or(CaptureError::EmptyArea)?;
        if area.is_empty() {
            return Err(CaptureError::EmptyArea);
        }

        unsafe {
            let screen = ScreenDc(GetDC(None));
            if screen.0.is_invalid() {
                return Err(CaptureError::Gdi("GetDC returned an invalid DC".into()));
            }

            let mem = MemDc(CreateCompatibleDC(Some(screen.0)));
            if mem.0.is_invalid() {
                return Err(CaptureError::Gdi("CreateCompatibleDC failed".into()));
            }

            let bitmap = Bitmap(CreateCompatibleBitmap(
                screen.0,
                area.width as i32,
                area.height as i32,
            ));
            if bitmap.0.is_invalid() {
                return Err(CaptureError::Gdi("CreateCompatibleBitmap failed".into()));
            }

            SelectObject(mem.0, HGDIOBJ(bitmap.0 .0));

            let len = area.width as usize * area.height as usize * 4;
            Ok(Self {
                screen,
                mem,
                bitmap,
                area,
                buffer: vec![0u8; len],
            })
        }
    }

    pub fn area(&self) -> Bounds {
        self.area
    }

    /// Grab one frame as top-down BGRA, into the buffer this grabber owns.
    pub fn grab(&mut self) -> Result<&[u8], CaptureError> {
        let width = self.area.width as i32;
        let height = self.area.height as i32;

        unsafe {
            BitBlt(
                self.mem.0,
                0,
                0,
                width,
                height,
                Some(self.screen.0),
                self.area.x,
                self.area.y,
                SRCCOPY,
            )
            .map_err(|e| CaptureError::Gdi(format!("BitBlt failed: {e}")))?;

            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative: top-down, which is what the encoder is told to
                    // expect. Flipping this silently mirrors every frame.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            let copied = GetDIBits(
                self.mem.0,
                self.bitmap.0,
                0,
                height as u32,
                Some(self.buffer.as_mut_ptr() as *mut c_void),
                &mut info,
                DIB_RGB_COLORS,
            );
            if copied == 0 {
                return Err(CaptureError::Gdi("GetDIBits copied no scanlines".into()));
            }

            // Screen content comes back with a zero alpha byte, which the
            // encoder would otherwise read as fully transparent.
            for pixel in self.buffer.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        }

        Ok(&self.buffer)
    }
}

/// Capture into a raw top-down BGRA buffer, exactly as GDI produces it.
///
/// Recording uses this rather than [`capture_area`]. Media Foundation wants
/// BGRA, so going via `RgbaImage` would mean swapping every pixel into RGBA and
/// straight back again — thirty times a second, for nothing.
pub fn capture_area_bgra(area: Bounds) -> Result<(Vec<u8>, Bounds), CaptureError> {
    grab(area, false)
}

/// Shared GDI grab. `swap_to_rgba` controls whether the BGRA bytes GDI returns
/// are reordered on the way out.
fn grab(area: Bounds, swap_to_rgba: bool) -> Result<(Vec<u8>, Bounds), CaptureError> {
    let desktop = virtual_desktop().as_rect();
    let area = area.intersect(desktop).ok_or(CaptureError::EmptyArea)?;

    if area.is_empty() {
        return Err(CaptureError::EmptyArea);
    }

    let width = area.width as i32;
    let height = area.height as i32;

    unsafe {
        // GetDC(None) yields a DC for the entire virtual screen, so a single
        // BitBlt can span every monitor at once.
        let screen = ScreenDc(GetDC(None));
        if screen.0.is_invalid() {
            return Err(CaptureError::Gdi("GetDC returned an invalid DC".into()));
        }

        let mem = MemDc(CreateCompatibleDC(Some(screen.0)));
        if mem.0.is_invalid() {
            return Err(CaptureError::Gdi("CreateCompatibleDC failed".into()));
        }

        // Must be compatible with the *screen* DC, not the memory DC — a bitmap
        // made compatible with a fresh memory DC would be 1bpp monochrome.
        let bitmap = Bitmap(CreateCompatibleBitmap(screen.0, width, height));
        if bitmap.0.is_invalid() {
            return Err(CaptureError::Gdi("CreateCompatibleBitmap failed".into()));
        }

        let previous = SelectObject(mem.0, HGDIOBJ(bitmap.0 .0));

        // CAPTUREBLT is what makes layered and transparent windows appear in the
        // result; without it they are punched out of the capture.
        let blit = BitBlt(
            mem.0,
            0,
            0,
            width,
            height,
            Some(screen.0),
            area.x,
            area.y,
            SRCCOPY | CAPTUREBLT,
        );

        // Restore the DC's original bitmap before the guards run, so the bitmap
        // we are about to delete is not still selected into a live DC.
        SelectObject(mem.0, previous);

        blit.map_err(|e| CaptureError::Gdi(format!("BitBlt failed: {e}")))?;

        let pixels = read_bitmap_pixels(mem.0, bitmap.0, width, height, swap_to_rgba)?;
        Ok((pixels, area))
    }
}

/// Read a GDI bitmap into a top-down byte buffer.
fn read_bitmap_pixels(
    dc: HDC,
    bitmap: HBITMAP,
    width: i32,
    height: i32,
    swap_to_rgba: bool,
) -> Result<Vec<u8>, CaptureError> {
    let mut header = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative height requests top-down row order, matching how the
            // `image` crate expects rows. Without this the capture is flipped.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let byte_count = width as usize * height as usize * 4;
    let mut buffer = vec![0u8; byte_count];

    let scanlines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(buffer.as_mut_ptr() as *mut c_void),
            &mut header,
            DIB_RGB_COLORS,
        )
    };

    if scanlines == 0 {
        return Err(CaptureError::Gdi("GetDIBits returned no scanlines".into()));
    }

    // GDI leaves the alpha byte as zero for screen content, so it always has to
    // be forced opaque — otherwise a PNG encoder writes a fully transparent
    // image, and the encoder sees garbage alpha. The channel swap is only for
    // callers that want RGBA; recording keeps the native BGRA order.
    if swap_to_rgba {
        for pixel in buffer.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
    } else {
        for pixel in buffer.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    }

    Ok(buffer)
}

/// Capture the whole virtual desktop in one read.
///
/// Region selection uses this to freeze the screen *before* showing its overlay,
/// so the image the user drags over is exactly the image they get — menus and
/// tooltips cannot move or close mid-selection.
pub fn capture_virtual_desktop() -> Result<Frame, CaptureError> {
    capture_area(virtual_desktop().as_rect())
}

/// A top-level window we could capture.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowTarget {
    pub bounds: Bounds,
    pub title: String,
}

/// Every capturable top-level window, topmost first.
///
/// The overlay uses this to highlight whichever window is under the cursor in
/// window mode. Z-order matters: overlapping windows mean the *first* match for
/// a point is the one the user is actually pointing at.
///
/// This is deliberately enumerated before the overlay is shown. Once the overlay
/// exists it is the topmost window on screen, and while `is_capturable` already
/// filters out our own process, snapshotting first also keeps the highlight
/// stable for the whole session rather than re-querying on every mouse move.
pub fn capturable_windows() -> Vec<WindowTarget> {
    let desktop = virtual_desktop().as_rect();
    let mut targets = Vec::new();

    unsafe {
        let mut current = GetTopWindow(None).unwrap_or_default();
        while !current.is_invalid() {
            if is_capturable(current) {
                if let Some(mut target) = describe_window(current) {
                    // A maximised window overhangs the work area slightly, and a
                    // window can be dragged half off-screen. Clip so the
                    // highlight never implies pixels we cannot read.
                    if let Some(clipped) = target.bounds.intersect(desktop) {
                        target.bounds = clipped;
                        targets.push(target);
                    }
                }
            }
            current = match GetWindow(current, GW_HWNDNEXT) {
                Ok(next) => next,
                Err(_) => break,
            };
        }
    }

    targets
}

/// Find the window the user means by "the active window".
///
/// The foreground window is usually the answer, but not when the capture was
/// triggered by clicking a button in our own UI — at that moment *we* are
/// foreground. So windows belonging to this process are skipped, and the search
/// falls through to the topmost real window behind us.
pub fn active_window() -> Result<WindowTarget, CaptureError> {
    unsafe {
        let foreground = GetForegroundWindow();
        if !foreground.is_invalid() && is_capturable(foreground) {
            if let Some(target) = describe_window(foreground) {
                return Ok(target);
            }
        }

        // Walk the Z-order from the top and take the first real window that is
        // not ours.
        let mut current = GetTopWindow(None).unwrap_or_default();
        while !current.is_invalid() {
            if is_capturable(current) {
                if let Some(target) = describe_window(current) {
                    return Ok(target);
                }
            }
            current = match GetWindow(current, GW_HWNDNEXT) {
                Ok(next) => next,
                Err(_) => break,
            };
        }
    }

    Err(CaptureError::NoForegroundWindow)
}

/// Whether a window is a genuine, visible, capturable target.
fn is_capturable(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }

        // Never capture our own windows — notably the region-selection overlay,
        // which is fullscreen and always-on-top.
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return false;
        }

        // Tool windows are palettes and tooltips, not what a user means by
        // "the active window".
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }

        // Windows 10/11 keep suspended UWP apps around as invisible "cloaked"
        // windows that are still reported as visible by IsWindowVisible.
        let mut cloaked = 0u32;
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        );
        if ok.is_ok() && cloaked != 0 {
            return false;
        }

        true
    }
}

fn describe_window(hwnd: HWND) -> Option<WindowTarget> {
    let bounds = window_bounds(hwnd)?;
    if bounds.is_empty() {
        return None;
    }

    let mut text = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut text) };
    let title = String::from_utf16_lossy(&text[..len.max(0) as usize]);

    Some(WindowTarget { bounds, title })
}

/// The window's visible frame in virtual-screen coordinates.
///
/// `GetWindowRect` includes the invisible resize border DWM adds around every
/// window, which would leave an ugly transparent margin in the capture.
/// `DWMWA_EXTENDED_FRAME_BOUNDS` reports the frame the user actually sees.
fn window_bounds(hwnd: HWND) -> Option<Bounds> {
    unsafe {
        let mut rect = RECT::default();
        let dwm = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        );

        if dwm.is_ok() {
            return Some(Bounds::from_rect(rect));
        }

        // Older or non-composited windows do not support the DWM attribute.
        let mut fallback = RECT::default();
        GetWindowRect(hwnd, &mut fallback).ok()?;
        Some(Bounds::from_rect(fallback))
    }
}

/// The monitor a point sits on, used to decide which display a region started on.
pub fn monitor_at(point: (i32, i32)) -> Option<MonitorInfo> {
    let p = POINT {
        x: point.0,
        y: point.1,
    };
    monitors().into_iter().find(|m| {
        p.x >= m.x && p.x < m.x + m.width as i32 && p.y >= m.y && p.y < m.y + m.height as i32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_clips_to_the_overlap() {
        let a = Bounds {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let b = Bounds {
            x: 50,
            y: 50,
            width: 100,
            height: 100,
        };
        assert_eq!(
            a.intersect(b),
            Some(Bounds {
                x: 50,
                y: 50,
                width: 50,
                height: 50
            })
        );
    }

    #[test]
    fn intersect_returns_none_when_disjoint() {
        let a = Bounds {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let b = Bounds {
            x: 100,
            y: 100,
            width: 10,
            height: 10,
        };
        assert_eq!(a.intersect(b), None);
    }

    #[test]
    fn intersect_handles_negative_origins() {
        // A second monitor placed to the left of the primary gives negative
        // virtual-screen coordinates, which is the case most likely to be got
        // wrong by accident.
        let desktop = Bounds {
            x: -1920,
            y: 0,
            width: 3840,
            height: 1080,
        };
        let selection = Bounds {
            x: -200,
            y: 100,
            width: 400,
            height: 200,
        };
        assert_eq!(selection.intersect(desktop), Some(selection));
    }

    #[test]
    fn crop_translates_out_of_virtual_screen_space() {
        // A frame that starts at -1920 should map a selection at -1920 to pixel 0.
        let image = RgbaImage::new(100, 100);
        let frame = Frame {
            image,
            origin: (-1920, -50),
        };
        let cropped = frame
            .crop(Bounds {
                x: -1920,
                y: -50,
                width: 10,
                height: 10,
            })
            .unwrap();
        assert_eq!(cropped.image.dimensions(), (10, 10));
        assert_eq!(cropped.origin, (-1920, -50));
    }

    #[test]
    fn crop_clips_a_selection_dragged_past_the_edge() {
        let frame = Frame {
            image: RgbaImage::new(100, 100),
            origin: (0, 0),
        };
        let cropped = frame
            .crop(Bounds {
                x: 90,
                y: 90,
                width: 50,
                height: 50,
            })
            .unwrap();
        assert_eq!(cropped.image.dimensions(), (10, 10));
    }
}
