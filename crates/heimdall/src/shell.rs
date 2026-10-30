//! Background COM workers for slow shell calls: icons, thumbnails, the app
//! list, Control Panel applets and recent documents. Results come back over a channel, and the UI
//! thread is nudged with a posted message.

use crate::apps::{self, App, Applet};
use std::sync::mpsc::{channel, Sender};
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_SHELL_DONE: u32 = 0x8000 + 20;

#[derive(Clone)]
pub struct Image {
    pub px: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

pub enum Job {
    Icon { key: String, path: String, px: u32 },
    Preview { path: String, px: u32 },
    Apps,
    /// Recent documents opened after this Unix time.
    Recent { after: u64 },
}

pub enum Done {
    Icon { key: String, img: Option<Image> },
    Preview(Box<Preview>),
    Apps(Vec<App>, Vec<Applet>),
    Recent(Vec<String>),
}

pub struct Worker {
    tx: Sender<Job>,
}

impl Worker {
    pub fn spawn(name: &str, hwnd: HWND, done: Sender<Done>) -> Worker {
        let (tx, rx) = channel::<Job>();
        let hwnd = hwnd.0 as isize;
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok().ok() };
                while let Ok(job) = rx.recv() {
                    let out = match job {
                        Job::Icon { key, path, px } => Done::Icon { key, img: shell_image(&path, px, SIIGBF_ICONONLY) },
                        Job::Preview { path, px } => Done::Preview(Box::new(preview(path, px))),
                        Job::Apps => Done::Apps(apps::enumerate(), apps::applets()),
                        Job::Recent { after } => Done::Recent(recent_files(16, after)),
                    };
                    if done.send(out).is_err() {
                        break;
                    }
                    unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_SHELL_DONE, WPARAM(0), LPARAM(0)).ok() };
                }
            })
            .ok();
        Worker { tx }
    }

    pub fn send(&self, job: Job) {
        self.tx.send(job).ok();
    }
}

pub struct Preview {
    pub path: String,
    pub image: Option<Image>,
    /// True when `image` is a real thumbnail rather than an icon.
    pub thumbnail: bool,
    pub dir: bool,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub lines: Vec<String>,
    pub children: Vec<String>,
    pub child_count: usize,
}

const TEXT_EXT: &[&str] = &[
    "txt", "md", "rs", "toml", "json", "yaml", "yml", "ini", "cfg", "conf", "log", "csv", "tsv", "py", "js", "ts",
    "tsx", "jsx", "c", "h", "cpp", "hpp", "cc", "cs", "java", "kt", "go", "rb", "php", "lua", "sh", "ps1", "bat",
    "cmd", "html", "htm", "css", "scss", "xml", "sql", "gitignore", "env", "hlsl", "glsl", "vue", "svelte", "zig",
];

fn preview(path: String, px: u32) -> Preview {
    let md = std::fs::metadata(&path).ok();
    let dir = md.as_ref().is_some_and(|m| m.is_dir());
    let mut p = Preview {
        image: None,
        thumbnail: false,
        dir,
        size: md.as_ref().filter(|m| !m.is_dir()).map(|m| m.len()),
        modified: md.as_ref().and_then(|m| m.modified().ok()).and_then(format_time),
        lines: Vec::new(),
        children: Vec::new(),
        child_count: 0,
        path,
    };
    let ext = std::path::Path::new(&p.path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if dir {
        if let Ok(rd) = std::fs::read_dir(&p.path) {
            let mut names: Vec<(bool, String)> = rd
                .flatten()
                .map(|e| (e.file_type().map(|t| !t.is_dir()).unwrap_or(true), e.file_name().to_string_lossy().into_owned()))
                .collect();
            p.child_count = names.len();
            names.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
            p.children = names.into_iter().take(14).map(|(file, n)| if file { n } else { format!("{n}\\") }).collect();
        }
    } else if TEXT_EXT.contains(&ext.as_str()) && p.size.is_some_and(|s| s < 8 << 20) {
        use std::io::Read;
        let mut buf = vec![0u8; 12 * 1024];
        if let Ok(mut f) = std::fs::File::open(&p.path) {
            let n = f.read(&mut buf).unwrap_or(0);
            buf.truncate(n);
            if !buf.contains(&0) {
                p.lines = String::from_utf8_lossy(&buf).lines().take(48).map(|l| l.replace('\t', "    ")).collect();
            }
        }
    }
    if p.lines.is_empty() {
        p.image = shell_image(&p.path, px, SIIGBF_THUMBNAILONLY);
        p.thumbnail = p.image.is_some();
        if p.image.is_none() {
            p.image = shell_image(&p.path, px.min(256), SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK);
        }
    }
    p
}

fn format_time(t: std::time::SystemTime) -> Option<String> {
    use windows::Win32::System::Time::*;
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    let ticks = d.as_nanos() as u64 / 100 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    unsafe {
        let mut utc = SYSTEMTIME::default();
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        let mut local = SYSTEMTIME::default();
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        Some(format!(
            "{} {} {}, {:02}:{:02}",
            local.wDay,
            MONTHS[(local.wMonth.clamp(1, 12) - 1) as usize],
            local.wYear,
            local.wHour,
            local.wMinute
        ))
    }
}

fn shell_image(path: &str, px: u32, flags: SIIGBF) -> Option<Image> {
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let factory: IShellItemImageFactory = item.cast().ok()?;
        let hbmp = factory.GetImage(SIZE { cx: px as i32, cy: px as i32 }, flags).ok()?;
        let img = bitmap_pixels(hbmp);
        DeleteObject(hbmp.into()).ok().ok();
        img
    }
}

/// Copy an HBITMAP into premultiplied, top-down BGRA.
unsafe fn bitmap_pixels(hbmp: HBITMAP) -> Option<Image> {
    let mut bm = BITMAP::default();
    if GetObjectW(hbmp.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth.max(1) as u32, bm.bmHeight.unsigned_abs().max(1));
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    let dc = GetDC(None);
    let lines = GetDIBits(dc, hbmp, 0, h, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    if lines == 0 {
        return None;
    }
    // Thumbnails of photos come back without alpha; icons usually carry it.
    // If a colour channel exceeds alpha the data is straight alpha: premultiply.
    let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
    if !has_alpha {
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    } else if px.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]) {
        for p in px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
    Some(Image { px, w, h })
}

/// Most recently used documents from the shell's Recent folder, newest first.
fn recent_files(max: usize, after: u64) -> Vec<String> {
    let after = std::time::UNIX_EPOCH + std::time::Duration::from_secs(after);
    let Some(dir) = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join(r"Microsoft\Windows\Recent")) else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut links: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(t, _)| *t > after)
        .collect();
    links.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = Vec::new();
    for (_, lnk) in links.into_iter().take(max * 3) {
        if let Some(t) = resolve_link(&lnk) {
            if std::path::Path::new(&t).exists() && !out.contains(&t) {
                out.push(t);
                if out.len() == max {
                    break;
                }
            }
        }
    }
    out
}

fn resolve_link(lnk: &std::path::Path) -> Option<String> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let pf: IPersistFile = link.cast().ok()?;
        pf.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).ok()?;
        let mut buf = [0u16; 520];
        link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_UNCPRIORITY.0 as u32).ok()?;
        let s = PWSTR(buf.as_mut_ptr()).to_string().ok()?;
        (!s.is_empty()).then_some(s)
    }
}
