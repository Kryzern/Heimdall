//! Installed applications, via the same `shell:AppsFolder` view the Start menu
//! uses. That covers classic desktop apps and Store/UWP apps in one list.

use crate::items::Target;
use windows::core::{w, GUID, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::*;

#[derive(Clone, Debug)]
pub struct App {
    pub name: String,
    /// Parsing name inside AppsFolder: an AUMID or a known-folder relative path.
    pub id: String,
    /// Real executable / shortcut path when one can be resolved.
    pub path: Option<String>,
    pub target: Target,
}

impl App {
    pub fn launch_uri(&self) -> String {
        format!("shell:AppsFolder\\{}", self.id)
    }
}

fn take(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string().unwrap_or_default() };
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

/// `{6D809377-...}\Microsoft VS Code\Code.exe` -> `C:\Program Files\Microsoft VS Code\Code.exe`
fn resolve(id: &str) -> Option<String> {
    let rest = id.strip_prefix('{')?;
    let (guid, tail) = rest.split_once("}\\")?;
    let guid = GUID::try_from(guid).ok()?;
    let base = unsafe { SHGetKnownFolderPath(&guid, KNOWN_FOLDER_FLAG(0), None).ok()? };
    Some(format!("{}\\{}", take(base), tail))
}

fn is_noise(name: &str, id: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("uninstall")
        || n.starts_with("readme")
        || n.ends_with(" help")
        || n.ends_with(" documentation")
        || n.ends_with(" website")
        || id.starts_with("http")
        || id.ends_with(".url")
        || id.ends_with(".txt")
        || id.ends_with(".html")
        || id.ends_with(".chm")
        || id.ends_with(".pdf")
}

/// A Control Panel applet ("Power Options", "Credential Manager", a driver's
/// own panel), from the same All Control Panel Items view Explorer shows, so
/// names are in the user's language.
#[derive(Clone, Debug)]
pub struct Applet {
    pub name: String,
    /// `shell:::{26EE0668-...}\0\::{CLSID}`, for launching and the icon.
    pub uri: String,
    pub target: Target,
}

/// Enumerate Control Panel applets. Call on a thread with COM initialised.
pub fn applets() -> Vec<Applet> {
    let mut out = Vec::new();
    unsafe {
        let Ok(folder) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(w!("shell:::{26EE0668-A00A-44D7-9371-BEB064C98683}\\0"), None)
        else {
            return out;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
        loop {
            let mut batch: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
            let mut got = 0u32;
            if items.Next(&mut batch, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            for item in batch.iter().take(got as usize).flatten() {
                let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).map(take).unwrap_or_default();
                let path = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).map(take).unwrap_or_default();
                if name.is_empty() || !path.starts_with("::") {
                    continue;
                }
                out.push(Applet { target: Target::new(&name), name, uri: format!("shell:{path}") });
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Enumerate apps. Call on a thread with COM initialised.
pub fn enumerate() -> Vec<App> {
    let mut out = Vec::new();
    unsafe {
        let folder = match SHCreateItemFromParsingName::<_, _, IShellItem>(w!("shell:AppsFolder"), None) {
            Ok(f) => f,
            Err(e) => {
                crate::log(format!("apps: AppsFolder unavailable: {e}"));
                return out;
            }
        };
        let items = match folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) {
            Ok(i) => i,
            Err(e) => {
                crate::log(format!("apps: enumerate failed: {e}"));
                return out;
            }
        };
        loop {
            let mut batch: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
            let mut got = 0u32;
            if let Err(e) = items.Next(&mut batch, Some(&mut got)) {
                crate::log(format!("apps: Next failed: {e}"));
                break;
            }
            if got == 0 {
                break;
            }
            for item in batch.iter().take(got as usize).flatten() {
                let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).map(take).unwrap_or_default();
                let id = item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING).map(take).unwrap_or_default();
                if name.is_empty() || id.is_empty() || is_noise(&name, &id) {
                    continue;
                }
                let path = resolve(&id).or_else(|| (id.contains(":\\")).then(|| id.clone()));
                out.push(App { target: Target::new(&name), name, id, path });
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.dedup_by(|a, b| a.name == b.name && a.path == b.path);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_panel_applets() {
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED).ok().ok();
        }
        let a = super::applets();
        assert!(a.len() > 10, "{} applets", a.len());
        assert!(a.iter().all(|x| x.uri.starts_with("shell:::{") && !x.name.is_empty()));
    }
}
