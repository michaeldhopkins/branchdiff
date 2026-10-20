//! Where a Chrome or Chromium binary lives, in the order a developer would expect.

use std::ffi::OsString;
use std::path::PathBuf;

const PATH_NAMES: [&str; 4] =
    ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"];
const SNAP: &str = "/snap/bin/chromium";
const MACOS_APPS: [&str; 3] = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
];
const WINDOWS_SUFFIX: &str = r"Google\Chrome\Application\chrome.exe";
const WINDOWS_ROOT_VARS: [&str; 3] = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"];

/// Every place to look, best first: `BRANCHDIFF_CHROME`, each `PATH` directory for each
/// known name, the snap, the macOS app bundles, then the Windows install roots.
pub fn candidates(
    override_path: Option<OsString>,
    path_var: Option<OsString>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = override_path.map(PathBuf::from).into_iter().collect();
    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(&path_var) {
            for name in PATH_NAMES {
                found.push(dir.join(name));
                found.push(dir.join(format!("{name}.exe")));
            }
        }
    }
    found.push(PathBuf::from(SNAP));
    found.extend(MACOS_APPS.map(PathBuf::from));
    for var in WINDOWS_ROOT_VARS {
        if let Some(root) = env(var) {
            found.push(PathBuf::from(root).join(WINDOWS_SUFFIX));
        }
    }
    found
}

/// The first candidate that exists, or why there is none.
pub fn locate(
    override_path: Option<OsString>,
    path_var: Option<OsString>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, String> {
    if let Some(named) = &override_path {
        let named = PathBuf::from(named);
        return if named.is_file() {
            Ok(named)
        } else {
            Err(format!("BRANCHDIFF_CHROME names {}, which is not a file", named.display()))
        };
    }
    candidates(None, path_var, env).into_iter().find(|p| p.is_file()).ok_or_else(|| {
        "no Chrome found: install Chrome or Chromium, or set BRANCHDIFF_CHROME".to_string()
    })
}

/// The first candidate that exists. A missing browser is a failure of the test that
/// asked, never a skip.
pub fn find_chrome() -> PathBuf {
    locate(std::env::var_os("BRANCHDIFF_CHROME"), std::env::var_os("PATH"), |name| {
        std::env::var_os(name)
    })
    .unwrap_or_else(|reason| panic!("{reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(name: &str) -> Option<OsString> {
        (name == "ProgramFiles").then(|| OsString::from(r"C:\Program Files"))
    }

    #[test]
    fn the_override_comes_first_then_path_then_the_fixed_locations() {
        let list = candidates(Some("/opt/mine".into()), Some("/usr/local/bin".into()), lookup);
        assert_eq!(list[0], PathBuf::from("/opt/mine"));
        let on_path = list.iter().position(|p| p == &PathBuf::from("/usr/local/bin/chromium"));
        let snap = list.iter().position(|p| p == &PathBuf::from(SNAP));
        let mac = list.iter().position(|p| p == &PathBuf::from(MACOS_APPS[0]));
        let windows =
            list.iter().position(|p| p == &PathBuf::from(r"C:\Program Files").join(WINDOWS_SUFFIX));
        assert!(on_path.unwrap() < snap.unwrap());
        assert!(snap < mac && mac < windows, "{list:?}");
    }

    #[test]
    fn an_override_that_names_nothing_is_an_error_naming_it_not_a_fallback() {
        let here = std::env::current_exe().unwrap();
        let found =
            locate(Some("/no/such/chrome".into()), Some(here.parent().unwrap().into()), lookup);
        let reason = found.expect_err("a bad override must not fall through to another browser");
        assert!(
            reason.contains("BRANCHDIFF_CHROME") && reason.contains("/no/such/chrome"),
            "{reason}"
        );
    }

    #[test]
    fn an_override_that_exists_is_used() {
        let here = std::env::current_exe().unwrap();
        assert_eq!(locate(Some(here.clone().into_os_string()), None, lookup), Ok(here));
    }

    #[test]
    fn every_documented_name_is_looked_for_on_path() {
        let list = candidates(None, Some("/bin".into()), lookup);
        for name in ["google-chrome", "chromium", "chromium-browser"] {
            assert!(list.contains(&PathBuf::from("/bin").join(name)), "{name}");
        }
    }
}
