//! The local shell behind a terminal tab (adapted from kubenx's local_shell).

use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

/// Starts `shell` (or `$SHELL`) as `-l -i` in `cwd`. Output arrives on the receiver, which ends when the shell exits.
pub fn spawn(shell: &str, cwd: &Path, size: PtySize) -> anyhow::Result<(Pty, UnboundedReceiver<Vec<u8>>)> {
    let pair = native_pty_system().openpty(size)?;
    let shell = Some(shell.trim().to_string()).filter(|shell| !shell.is_empty());
    let shell = shell.or_else(|| std::env::var("SHELL").ok()).unwrap_or_else(|| "/bin/zsh".into());
    let mut cmd = CommandBuilder::new(shell);
    // Login + interactive, like Terminal.app: a Finder-launched app only has `/usr/bin:/bin`.
    cmd.args(["-l", "-i"]);
    cmd.env("PATH", login_path());
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "nxbrowser");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(dir) = Some(cwd).filter(|dir| dir.is_dir()).map(Path::to_path_buf).or(home) {
        cmd.cwd(dir);
    }
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let (tx, rx) = unbounded();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.unbounded_send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    Ok((Pty { master: pair.master, writer, child }, rx))
}

impl Pty {
    pub fn write(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.writer.write_all(bytes);
            let _ = self.writer.flush();
        }
    }

    pub fn resize(&self, size: PtySize) {
        let _ = self.master.resize(size);
    }

    /// The shell's working directory, so a restored tab starts where it was left.
    pub fn cwd(&self) -> Option<PathBuf> {
        let pid = self.child.process_id()? as libc::c_int;
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        // SAFETY: `info` is a plain C struct valid for `size` bytes for the whole call.
        let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
        if got != size {
            return None;
        }
        // SAFETY: the kernel fills `vip_path` with a NUL-terminated path.
        let path = unsafe { std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
        Some(PathBuf::from(path.to_string_lossy().into_owned())).filter(|path| path.is_absolute())
    }

    /// The program in the foreground, when it is not the shell itself (`vim`, `htop`).
    pub fn foreground(&self) -> Option<String> {
        let group = self.master.process_group_leader().filter(|pid| *pid > 0)?;
        if self.child.process_id().is_some_and(|shell| shell as libc::pid_t == group) {
            return None;
        }
        let mut buf = [0u8; 64];
        // SAFETY: the buffer is valid for `buf.len()` bytes for the whole call.
        let len = unsafe { libc::proc_name(group, buf.as_mut_ptr().cast(), buf.len() as u32) };
        let len = usize::try_from(len).ok().filter(|len| *len > 0)?;
        Some(String::from_utf8_lossy(&buf[..len]).into_owned())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// PATH a GUI process is missing: Homebrew, Cargo and the usual user bins, then what the app inherited.
fn login_path() -> OsString {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend([home.join(".local/bin"), home.join(".cargo/bin"), home.join("go/bin")]);
    }
    dirs.extend(["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    let mut path: Vec<PathBuf> = dirs.into_iter().filter(|dir| dir.is_dir()).collect();
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if !dir.as_os_str().is_empty() && !path.contains(&dir) {
            path.push(dir);
        }
    }
    std::env::join_paths(path).unwrap_or_else(|_| "/usr/bin:/bin".into())
}
