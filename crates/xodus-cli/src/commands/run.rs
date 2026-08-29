use std::collections::HashMap;
use std::os::fd::{AsFd, IntoRawFd};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use msixvc::layout::PAGE_SIZE;
use msixvc::xvd::{SegmentFile, XvdFile};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
#[cfg(target_os = "linux")]
use rustix::fs::{MemfdFlags, memfd_create};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
#[cfg(not(target_os = "linux"))]
use tempfile::{tempdir, tempfile, tempfile_in};
use tokio::fs::{File, OpenOptions};
use tokio::process::Command;
use xodus::tokens::TokenManager;

use crate::license::get_license;

#[cfg(target_os = "linux")]
fn make_temp_file(_folder: &str) -> std::io::Result<std::fs::File> {
    let fd = memfd_create("xodus", MemfdFlags::CLOEXEC).map_err(std::io::Error::from)?;
    Ok(std::fs::File::from(fd))
}

#[cfg(not(target_os = "linux"))]
fn make_temp_file(folder: &str) -> std::io::Result<std::fs::File> {
    if folder.is_empty() {
        tempfile()
    } else {
        tempfile_in(folder)
    }
}

#[cfg(target_os = "macos")]
async fn prepare(lfiles: &HashMap<String, SegmentFile>) -> (impl AsyncFnOnce(), String) {
    let disk_size: u64 = lfiles
        .iter()
        .filter(|f| f.1.keep_encrypted)
        .map(|f| f.1.length + 4 * PAGE_SIZE as u64)
        .reduce(|o, s| o + s)
        .unwrap();

    let device_s = String::from_utf8(
        Command::new("/usr/bin/hdiutil")
            .arg("attach")
            .arg("-nomount")
            .arg(format!("ram://{}", disk_size.div_ceil(256)))
            .output()
            .await
            .unwrap()
            .stdout,
    )
    .unwrap();

    let device = device_s.trim();

    let vol = uuid::Uuid::new_v4().to_string();

    let fmt = Command::new("/sbin/newfs_hfs")
        .arg("-v")
        .arg(vol)
        .arg(device)
        .status()
        .await
        .unwrap();
    assert!(fmt.success());

    let mount_dir_obj = tempdir().unwrap();
    let mount_dir = mount_dir_obj.path().to_str().unwrap();

    let mnt = Command::new("/sbin/mount")
        .arg("-t")
        .arg("hfs")
        .arg("-o")
        .arg("nobrowse")
        .arg("-v")
        .arg(device)
        .arg(mount_dir)
        .status()
        .await
        .unwrap();
    assert!(mnt.success());
    let mount_dir_cl = mount_dir.to_string();
    let device_cl = device.to_string();
    (
        async move || {
            let mnt = Command::new("/sbin/umount")
                .arg("-f")
                .arg(mount_dir_cl)
                .status()
                .await
                .unwrap();
            assert!(mnt.success());

            let mnt = Command::new("/usr/bin/hdiutil")
                .arg("detach")
                .arg("-force")
                .arg(&device_cl)
                .status()
                .await
                .unwrap();
            assert!(mnt.success());
        },
        mount_dir.to_owned(),
    )
}

#[cfg(not(target_os = "macos"))]
async fn prepare(_lfiles: &HashMap<String, SegmentFile>) -> (impl AsyncFnOnce(), String) {
    (async || {}, "".to_owned())
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|p| is_executable_file(p))
}

/// Resolution order: explicit CLI argument, `$XODUS_WINE`, `xodus-wine` on
/// PATH, plain `wine` on PATH. The last one only works with a wine build
/// patched to honor `WINE_DLL_FILE_MAP` (see docs/xodus/wine.md) - stock wine
/// would try to load the still-encrypted on-disk executables - so falling
/// back to it prints a warning instead of failing silently at game startup.
fn resolve_wine(explicit: Option<String>) -> Result<PathBuf, String> {
    fn named(value: &str, origin: &str) -> Result<PathBuf, String> {
        let path = Path::new(value);
        if path.components().count() > 1 {
            if is_executable_file(path) {
                Ok(path.to_path_buf())
            } else {
                Err(format!("{origin} `{value}` is not an executable file"))
            }
        } else {
            find_in_path(value).ok_or_else(|| format!("{origin} `{value}` was not found in PATH"))
        }
    }

    if let Some(value) = explicit {
        return named(&value, "wine argument");
    }
    if let Ok(value) = std::env::var("XODUS_WINE") {
        return named(&value, "XODUS_WINE");
    }
    if let Some(found) = find_in_path("xodus-wine") {
        return Ok(found);
    }
    if let Some(found) = find_in_path("wine") {
        eprintln!(
            "warning: falling back to `{}` from PATH; running games requires a wine build \
             with WINE_DLL_FILE_MAP support (see docs/xodus/wine.md) - stock wine will fail \
             to load the encrypted executables",
            found.display()
        );
        return Ok(found);
    }
    Err(
        "no wine found: pass a path as the second argument, set XODUS_WINE, or put \
         `xodus-wine` (or `wine`) in PATH; see docs/xodus/wine.md for the required build"
            .to_string(),
    )
}

/// Package-internal paths are backslash-separated and case-insensitive
/// (NTFS/Windows semantics); user input may use either separator and any case.
fn normalize_internal_path(path: &str) -> String {
    path.replace('/', "\\")
        .trim_start_matches('\\')
        .to_ascii_lowercase()
}

fn matches_exe(internal_name: &str, wanted: &str) -> bool {
    let name = normalize_internal_path(internal_name);
    let wanted = normalize_internal_path(wanted);
    name == wanted || name.ends_with(&format!("\\{wanted}"))
}

struct ServiceHandle {
    socket: PathBuf,
    // Present only when this launch spawned the service (it is stopped again
    // once the game exits); None means one was already running.
    child: Option<tokio::process::Child>,
}

fn find_service_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("xodus-service");
        if is_executable_file(&sibling) {
            return Some(sibling);
        }
    }
    find_in_path("xodus-service")
}

/// Connects to a running xodus-service or spawns one, so the game's
/// xgameruntime side has an IPC endpoint. Best-effort: a launch without the
/// service still works, Xbox Live services are just unavailable in-game.
async fn ensure_service() -> Option<ServiceHandle> {
    let Some(socket) = xodus::ipc::socket_path() else {
        eprintln!("warning: XDG_RUNTIME_DIR is not set; running without xodus-service");
        return None;
    };

    if xodus::ipc::ping(&socket, Duration::from_secs(2))
        .await
        .is_ok()
    {
        return Some(ServiceHandle {
            socket,
            child: None,
        });
    }

    let Some(binary) = find_service_binary() else {
        eprintln!(
            "warning: xodus-service not found next to xodus-cli or in PATH; running without \
             it - Xbox Live services will be unavailable in-game"
        );
        return None;
    };

    let mut child = match Command::new(&binary).spawn() {
        Ok(child) => child,
        Err(err) => {
            eprintln!(
                "warning: failed to start `{}`: {}; running without xodus-service",
                binary.display(),
                err
            );
            return None;
        }
    };

    // Startup includes device (re-)authentication over the network, so allow
    // a generous window before giving up.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if xodus::ipc::ping(&socket, Duration::from_secs(2))
            .await
            .is_ok()
        {
            return Some(ServiceHandle {
                socket,
                child: Some(child),
            });
        }
        if let Ok(Some(status)) = child.try_wait() {
            eprintln!(
                "warning: xodus-service exited during startup ({}); running without it",
                status
            );
            return None;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("warning: xodus-service did not come up in time; running without it");
            let _ = child.start_kill();
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn stop_service(handle: Option<ServiceHandle>) {
    let Some(ServiceHandle {
        child: Some(mut child),
        ..
    }) = handle
    else {
        return;
    };
    // SIGINT gives the service its clean-shutdown path (socket file removal);
    // escalate only if it does not exit.
    if let Some(pid) = child.id() {
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGINT);
    }
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

pub async fn run(
    client: &reqwest::Client,
    tokens: &TokenManager,
    source: String,
    wine: Option<String>,
    exe: Option<String>,
    market: Option<String>,
) -> ExitCode {
    let wine = match resolve_wine(wine) {
        Ok(wine) => wine,
        Err(err) => {
            eprintln!("{}", err);
            return ExitCode::FAILURE;
        }
    };

    let mut lfiles: HashMap<String, SegmentFile> = HashMap::new();

    let out: &Path = Path::new(&source);
    let out_absolute = match std::fs::canonicalize(out) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("cannot open game directory `{}`: {}", source, err);
            return ExitCode::FAILURE;
        }
    };
    let final_path = out.join(".xodus-streaming.msixvc");

    let mut file = match OpenOptions::new().read(true).open(&final_path).await {
        Ok(file) => file,
        Err(err) => {
            eprintln!(
                "cannot open `{}`: {}\nthe game directory must be an install produced by \
                 `xodus-cli streaming` (only streaming installs are runnable today)",
                final_path.display(),
                err
            );
            return ExitCode::FAILURE;
        }
    };

    let xvd = XvdFile::parse(&mut file).await.expect("no err");

    let files = xvd.parse_user_package_files(&mut file).await.expect("ok");
    for (k, v) in &files {
        if k == "SegmentMetadata.bin" {
            let sfiles = xvd.parse_segment_metadata(&mut file, v).await.expect("ok");
            lfiles = sfiles;
        }
    }

    // Classic files
    if lfiles.is_empty() {
        let sfiles = xvd
            .parse_ntfs_segment_metadata(&mut file, !lfiles.is_empty())
            .await
            .expect("ok");
        for (n, sfile) in &sfiles {
            if sfile.length.div_ceil(PAGE_SIZE as u64) as usize != sfile.data_hashs.len() {
                println!("{}: {} {}", n, sfile.offset, sfile.length);
            }
        }
        lfiles.extend(sfiles);
    }

    let license = get_license(
        client,
        tokens,
        xvd.content_id().to_string(),
        market.unwrap_or("neutral".to_string()),
    )
    .await;
    if let Err(err) = license {
        eprintln!("{}", err);
        return ExitCode::FAILURE;
    }
    let (key, game_splicense) = license.unwrap();
    if game_splicense.content_keys.len() != 1 {
        eprintln!(
            "unexpected number of content keys {}",
            game_splicense.content_keys.len()
        );
        return ExitCode::FAILURE;
    }
    let Some((_, content_key)) = game_splicense.content_keys.into_iter().next() else {
        return ExitCode::FAILURE;
    };

    let full_key = content_key.unpack(&key).expect("failed to unpack");

    let mut fds = vec![];

    let (cleanup, mount_dir) = prepare(&lfiles).await;

    for file in &lfiles {
        if !file.1.keep_encrypted {
            continue;
        }
        let mut game_exe = File::from_std(make_temp_file(&mount_dir).unwrap());

        let source_path = out.join(file.0.replace("\\", "/"));

        let mut i = match File::open(&source_path).await {
            Ok(i) => i,
            Err(err) => {
                eprintln!(
                    "cannot open `{}` (listed in the package metadata): {}\nthe install \
                     appears incomplete - re-run `xodus-cli streaming` for this game",
                    source_path.display(),
                    err
                );
                return ExitCode::FAILURE;
            }
        };

        xvd.mount_mem_fd(&mut i, &mut game_exe, file.1, *full_key, |_, _| {})
            .await
            .unwrap();

        let stdf = game_exe.into_std().await;

        let mut flags = fcntl_getfd(stdf.as_fd()).unwrap();
        flags.remove(FdFlags::CLOEXEC);
        fcntl_setfd(stdf.as_fd(), flags).unwrap();

        fds.push((file.0, stdf.into_raw_fd()));
    }

    // HashMap iteration order is random per run; sort so the same package
    // always maps (and, below, selects) the same way.
    fds.sort_by(|a, b| a.0.cmp(b.0));

    let mut env_value = String::new();
    let nt_prefix = out_absolute.to_string_lossy().replace("/", "\\");
    let nt_prefix = nt_prefix.trim_end_matches('\\');

    let mut nt_paths = vec![];
    for fd in &fds {
        if !env_value.is_empty() {
            env_value.push('|');
        }

        let nt_suffix = fd.0.trim_start_matches('\\');
        let nt_path = format!("\\??\\Z:{}\\{}", nt_prefix, nt_suffix);
        env_value.push_str(&format!("{}:{}", fd.1, nt_path));
        nt_paths.push((fd.0.as_str(), nt_path));
    }

    let nt_entry = if let Some(exe) = &exe {
        match nt_paths.iter().find(|(name, _)| matches_exe(name, exe)) {
            Some((_, nt_path)) => nt_path.clone(),
            None => {
                eprintln!("`{}` does not match any executable in this package:", exe);
                for (name, _) in &nt_paths {
                    eprintln!("  {}", name);
                }
                return ExitCode::FAILURE;
            }
        }
    } else {
        let exes: Vec<&(&str, String)> = nt_paths
            .iter()
            .filter(|(name, _)| name.to_ascii_lowercase().ends_with(".exe"))
            .collect();
        match exes.as_slice() {
            [] => {
                eprintln!("no .exe found among the package's encrypted files");
                return ExitCode::FAILURE;
            }
            [(_, nt_path)] => nt_path.clone(),
            _ => {
                eprintln!("this package contains multiple executables; pick one with --exe:");
                for (name, _) in &exes {
                    eprintln!("  --exe '{}'", name);
                }
                return ExitCode::FAILURE;
            }
        }
    };

    let service = ensure_service().await;

    let mut command = Command::new(&wine);
    command.arg(nt_entry).env("WINE_DLL_FILE_MAP", env_value);
    if let Some(service) = &service {
        // Contract for the wine-side runtime: XODUS_SOCKET names the
        // xodus-service IPC endpoint (see docs/xodus/service.md).
        command.env("XODUS_SOCKET", &service.socket);
    }
    let mut wn = match command.spawn() {
        Ok(wn) => wn,
        Err(err) => {
            eprintln!("failed to start `{}`: {}", wine.display(), err);
            stop_service(service).await;
            return ExitCode::FAILURE;
        }
    };

    let pid = wn.id().unwrap();

    ctrlc::set_handler(move || {
        if pid > 0 {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGINT);
        }
    })
    .expect("failed to install Ctrl+C handler");

    let status = wn.wait().await;

    cleanup().await;
    stop_service(service).await;

    let status = match status {
        Ok(status) => status,
        Err(err) => {
            eprintln!("failed to wait for wine: {}", err);
            return ExitCode::FAILURE;
        }
    };

    match status.code() {
        Some(code) => ExitCode::from(code as u8),
        // Signal death (crash, kill) previously mapped to exit 0; report it
        // with the shell convention instead.
        None => {
            let signal = status.signal().unwrap_or(0);
            eprintln!("wine terminated by signal {}", signal);
            ExitCode::from(128u8.wrapping_add(signal as u8))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{matches_exe, resolve_wine};

    #[test]
    fn explicit_wine_path_must_be_executable() {
        let err = resolve_wine(Some("/nonexistent/xodus-wine".to_string())).unwrap_err();
        assert!(err.contains("/nonexistent/xodus-wine"), "{err}");
    }

    #[test]
    fn exe_matching_is_case_and_separator_insensitive() {
        let internal = "\\Content\\Game\\Binaries\\Win64\\Game.exe";
        assert!(matches_exe(internal, "game.exe"));
        assert!(matches_exe(internal, "Win64/Game.exe"));
        assert!(matches_exe(
            internal,
            "content\\game\\binaries\\win64\\game.exe"
        ));
        assert!(matches_exe(
            internal,
            "/Content/Game/Binaries/Win64/Game.exe"
        ));
        assert!(!matches_exe(internal, "othergame.exe"));
        // suffix must match at a path-component boundary
        assert!(!matches_exe(internal, "ame.exe"));
    }
}
