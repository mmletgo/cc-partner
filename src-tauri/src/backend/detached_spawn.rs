//! detached_spawn — 让后台进程脱离 macOS GUI 责任链。
//!
//! Business Logic（为什么需要这个模块）:
//!     Dev.app 热更新 `codesign --force` 会让内核以 `SIGKILL (Code Signature Invalid)`
//!     杀掉 GUI **责任链**上的全部进程。仅 `setsid`/`setpgid` 或把 backend 拷到
//!     `data_dir/runtime` 不够：`responsibility_get_pid_responsible_for_pid` 仍指向 GUI。
//!     结果是 sidecar 与工作台 tmux server 被一起杀掉，restore 只能看到 `tmux_target_missing`。
//!
//! Code Logic（这个模块做什么）:
//!     macOS 上用 `posix_spawn` + `responsibility_spawnattrs_setdisclaim` 拉起子进程；
//!     其它 Unix 仍走 `setsid`。提供可 wait/kill 的 child 句柄。

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::ExitStatus;
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
use std::time::{Duration, Instant};

/// 脱离父应用责任链后拉起的子进程。
///
/// Business Logic（为什么需要这个结构）:
///     `start` 需要 pid 与 control 文件对账，并在失败路径有界 kill+reap。
///
/// Code Logic（这个结构做什么）:
///     记录 pid；macOS 用 waitpid/kill，其它平台包一层 `std::process::Child`。
pub struct DisclaimedChild {
    pid: u32,
    reaped: bool,
    #[cfg(not(target_os = "macos"))]
    inner: Option<std::process::Child>,
}

impl DisclaimedChild {
    /// 子进程 pid。
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// 非阻塞 reap。
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.reaped {
            return Ok(None);
        }
        self.try_wait_impl()
    }

    /// 发送 SIGKILL / Terminate。
    pub fn kill(&mut self) -> io::Result<()> {
        self.kill_impl()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     start 失败路径必须有界杀掉自己 spawn 的 serve，避免双 writer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     kill 后在 timeout 内 try_wait；确认退出返回 Ok 诊断，否则 Err 含残留 pid。
    pub fn kill_and_reap(&mut self, timeout: Duration) -> Result<String, String> {
        let pid = self.pid;
        let _ = self.kill();
        let deadline = Instant::now() + timeout;
        loop {
            match self.try_wait() {
                Ok(Some(status)) => {
                    return Ok(format!("；已终止子进程 pid={pid} status={status}"));
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return Err(format!("无法确认清理子进程，残留 pid={pid}"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => {
                    return Err(format!("reap 子进程 pid={pid} 失败: {err}"));
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn try_wait_impl(&mut self) -> io::Result<Option<ExitStatus>> {
        use std::os::unix::process::ExitStatusExt;
        let mut status: libc::c_int = 0;
        let result = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if result == 0 {
            Ok(None)
        } else if result == self.pid as libc::pid_t {
            self.reaped = true;
            Ok(Some(ExitStatus::from_raw(status)))
        } else if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(None)
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn try_wait_impl(&mut self) -> io::Result<Option<ExitStatus>> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(None);
        };
        let status = inner.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    #[cfg(unix)]
    fn kill_impl(&mut self) -> io::Result<()> {
        let result = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(unix))]
    fn kill_impl(&mut self) -> io::Result<()> {
        self.inner
            .as_mut()
            .ok_or_else(|| io::Error::other("missing child"))?
            .kill()
    }

    #[cfg(target_os = "macos")]
    fn wait_blocking(&mut self) -> io::Result<ExitStatus> {
        use std::os::unix::process::ExitStatusExt;
        let mut status: libc::c_int = 0;
        let result = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, 0) };
        if result == self.pid as libc::pid_t {
            self.reaped = true;
            Ok(ExitStatus::from_raw(status))
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn wait_blocking(&mut self) -> io::Result<ExitStatus> {
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| io::Error::other("missing child"))?;
        let status = inner.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

/// Business Logic（为什么需要这个函数）:
///     sidecar `serve` 与 tmux `start-server` 必须从 GUI 责任链断开，否则 Dev.app
///     重建签名会把 pane 里的 Agent 一起杀掉。
///
/// Code Logic（这个函数做什么）:
///     macOS：`posix_spawn` + `responsibility_spawnattrs_setdisclaim` + SETSID
///     + stdio `/dev/null`（rust libc 未导出 `POSIX_SPAWN_SETSID`，用 Darwin `0x0400`；
///     不可再叠加 SETPGROUP，否则 posix_spawnp EPERM）；
///     其它 Unix：`setsid` pre_exec。
pub fn spawn_disclaimed(
    program: impl AsRef<Path>,
    args: &[impl AsRef<OsStr>],
) -> io::Result<DisclaimedChild> {
    spawn_disclaimed_impl(program.as_ref(), args)
}

/// Business Logic（为什么需要这个函数）:
///     tmux `start-server` 是短生命周期 client，spawn 后应等到它退出，server 自行 daemonize。
///
/// Code Logic（这个函数做什么）:
///     `spawn_disclaimed` 后阻塞 wait。
pub fn run_disclaimed(
    program: impl AsRef<Path>,
    args: &[impl AsRef<OsStr>],
) -> io::Result<ExitStatus> {
    let mut child = spawn_disclaimed(program, args)?;
    child.wait_blocking()
}

/// Business Logic（为什么需要这个函数）:
///     单测要断言子进程不再把 GUI/调用者当作 responsible pid。
///
/// Code Logic（这个函数做什么）:
///     调用 `responsibility_get_pid_responsible_for_pid`；非 macOS 返回 None。
pub fn macos_responsible_pid(pid: u32) -> Option<u32> {
    macos_responsible_pid_impl(pid)
}

#[cfg(target_os = "macos")]
fn spawn_disclaimed_impl(
    program: &Path,
    args: &[impl AsRef<OsStr>],
) -> io::Result<DisclaimedChild> {
    use std::ffi::CString;

    unsafe extern "C" {
        fn responsibility_spawnattrs_setdisclaim(
            attr: *mut libc::posix_spawnattr_t,
            disclaim: libc::c_int,
        ) -> libc::c_int;
    }

    let program_c = cstring_from_os(program.as_os_str())?;
    let mut argv_c = Vec::with_capacity(args.len() + 2);
    argv_c.push(program_c.clone());
    for arg in args {
        argv_c.push(cstring_from_os(arg.as_ref())?);
    }
    let mut argv_ptrs: Vec<*mut libc::c_char> = argv_c
        .iter()
        .map(|s| s.as_ptr() as *mut libc::c_char)
        .collect();
    argv_ptrs.push(std::ptr::null_mut());

    let mut env_c = Vec::new();
    for (key, value) in std::env::vars_os() {
        let mut pair = key;
        pair.push("=");
        pair.push(&value);
        env_c.push(cstring_from_os(&pair)?);
    }
    let mut env_ptrs: Vec<*mut libc::c_char> = env_c
        .iter()
        .map(|s| s.as_ptr() as *mut libc::c_char)
        .collect();
    env_ptrs.push(std::ptr::null_mut());

    let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
    let mut actions: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
    check_posix(
        unsafe { libc::posix_spawnattr_init(&mut attr) },
        "posix_spawnattr_init",
    )?;
    let attr_guard = SpawnAttrGuard { attr: &mut attr };
    check_posix(
        unsafe { libc::posix_spawn_file_actions_init(&mut actions) },
        "posix_spawn_file_actions_init",
    )?;
    let actions_guard = SpawnActionsGuard {
        actions: &mut actions,
    };

    // Darwin sys/spawn.h：POSIX_SPAWN_SETSID = 0x0400。SETSID 已隐含新 process
    // group；再叠加 SETPGROUP 会 posix_spawnp EPERM。
    const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;
    let flags = POSIX_SPAWN_SETSID;
    check_posix(
        unsafe { libc::posix_spawnattr_setflags(attr_guard.attr, flags) },
        "posix_spawnattr_setflags",
    )?;
    check_posix(
        unsafe { responsibility_spawnattrs_setdisclaim(attr_guard.attr, 1) },
        "responsibility_spawnattrs_setdisclaim",
    )?;

    let dev_null = CString::new("/dev/null").expect("/dev/null");
    check_posix(
        unsafe {
            libc::posix_spawn_file_actions_addopen(
                actions_guard.actions,
                0,
                dev_null.as_ptr(),
                libc::O_RDWR,
                0,
            )
        },
        "posix_spawn_file_actions_addopen",
    )?;
    check_posix(
        unsafe { libc::posix_spawn_file_actions_adddup2(actions_guard.actions, 0, 1) },
        "posix_spawn_file_actions_adddup2 stdout",
    )?;
    check_posix(
        unsafe { libc::posix_spawn_file_actions_adddup2(actions_guard.actions, 0, 2) },
        "posix_spawn_file_actions_adddup2 stderr",
    )?;

    let mut pid: libc::pid_t = 0;
    check_posix(
        unsafe {
            libc::posix_spawnp(
                &mut pid,
                program_c.as_ptr(),
                actions_guard.actions,
                attr_guard.attr,
                argv_ptrs.as_ptr(),
                env_ptrs.as_ptr(),
            )
        },
        "posix_spawnp",
    )?;
    drop(actions_guard);
    drop(attr_guard);
    Ok(DisclaimedChild {
        pid: pid as u32,
        reaped: false,
    })
}

#[cfg(not(target_os = "macos"))]
fn spawn_disclaimed_impl(
    program: &Path,
    args: &[impl AsRef<OsStr>],
) -> io::Result<DisclaimedChild> {
    let mut command = std::process::Command::new(program);
    command
        .args(args.iter().map(AsRef::as_ref))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let inner = command.spawn()?;
    Ok(DisclaimedChild {
        pid: inner.id(),
        reaped: false,
        inner: Some(inner),
    })
}

#[cfg(target_os = "macos")]
fn cstring_from_os(value: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "spawn 参数含 NUL"))
}

#[cfg(target_os = "macos")]
fn check_posix(code: libc::c_int, what: &str) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
            .map_err(|err| io::Error::new(err.kind(), format!("{what}: {err}")))
    }
}

#[cfg(target_os = "macos")]
struct SpawnAttrGuard<'a> {
    attr: &'a mut libc::posix_spawnattr_t,
}

#[cfg(target_os = "macos")]
impl Drop for SpawnAttrGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawnattr_destroy(self.attr);
        }
    }
}

#[cfg(target_os = "macos")]
struct SpawnActionsGuard<'a> {
    actions: &'a mut libc::posix_spawn_file_actions_t,
}

#[cfg(target_os = "macos")]
impl Drop for SpawnActionsGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawn_file_actions_destroy(self.actions);
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_responsible_pid_impl(pid: u32) -> Option<u32> {
    unsafe extern "C" {
        fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
    }
    let responsible = unsafe { responsibility_get_pid_responsible_for_pid(pid as libc::pid_t) };
    if responsible <= 0 {
        None
    } else {
        Some(responsible as u32)
    }
}

#[cfg(not(target_os = "macos"))]
fn macos_responsible_pid_impl(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     Dev.app codesign SIGKILL 会杀掉 GUI 责任链上的进程。spawn 出的后台进程
    ///     必须把自己当作 responsible pid，不能再指向调用者。
    ///
    /// Code Logic（这个测试做什么）:
    ///     spawn `/bin/sleep`，断言 `responsibility_get_pid_responsible_for_pid(child) == child`。
    #[cfg(target_os = "macos")]
    #[test]
    fn spawn_disclaimed_child_is_its_own_responsible_pid() {
        let mut child = spawn_disclaimed("/bin/sleep", &["30"]).expect("spawn sleep");
        let pid = child.id();
        let responsible = macos_responsible_pid(pid).expect("应能读取 responsible pid");
        let _ = child.kill();
        let _ = child.try_wait();
        assert_eq!(
            responsible, pid,
            "disclaim 后 child 的 responsible pid 必须是它自己，实际 responsible={responsible} parent={}",
            std::process::id()
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     仅 SETPGROUP/disclaim 时 child 仍在 GUI 的 Unix session 里，GUI 退出或
    ///     codesign SIGKILL 会间歇性带走 tmux。daemonize 必须 setsid 成为 session leader。
    ///
    /// Code Logic（这个测试做什么）:
    ///     spawn `/bin/sleep`，断言 `getsid(child) == child`。
    #[cfg(unix)]
    #[test]
    fn spawn_disclaimed_child_is_session_leader() {
        let mut child = spawn_disclaimed("/bin/sleep", &["30"]).expect("spawn sleep");
        let pid = child.id() as libc::pid_t;
        let sid = unsafe { libc::getsid(pid) };
        let parent_sid = unsafe { libc::getsid(0) };
        let _ = child.kill();
        let _ = child.try_wait();
        assert_eq!(
            sid, pid,
            "disclaim spawn 必须 setsid，实际 sid={sid} pid={pid} parent_sid={parent_sid}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台 tmux server 若仍由 GUI 负责，codesign SIGKILL 会拆掉全部 pane。
    ///
    /// Code Logic（这个测试做什么）:
    ///     disclaim 后 `tmux start-server`，断言 server 进程的 responsible pid 是它自己。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_disclaimed_tmux_start_server_owns_its_responsibility() {
        let tmux = match std::process::Command::new("tmux").arg("-V").output() {
            Ok(output) if output.status.success() => "tmux",
            _ => return,
        };
        let dir =
            std::env::temp_dir().join(format!("cc-partner-disclaim-tmux-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp tmux dir");
        let sock = dir.join("cc-partner.sock");
        let conf = dir.join("tmux.conf");
        std::fs::write(
            &conf,
            "set -s exit-empty off\nset -g destroy-unattached off\n",
        )
        .expect("write tmux.conf");
        let status = run_disclaimed(
            tmux,
            &[
                "-S",
                sock.to_str().expect("sock utf8"),
                "-f",
                conf.to_str().expect("conf utf8"),
                "start-server",
            ],
        )
        .expect("disclaimed tmux start-server");
        assert!(status.success(), "start-server 应成功: {status}");
        let ps = std::process::Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()
            .expect("ps");
        let text = String::from_utf8_lossy(&ps.stdout);
        let sock_text = sock.to_string_lossy();
        let server_pid = text.lines().find_map(|line| {
            if !line.contains(sock_text.as_ref()) {
                return None;
            }
            line.split_whitespace().next()?.parse::<u32>().ok()
        });
        let server_pid = server_pid.expect("应能找到 tmux server pid");
        let responsible =
            macos_responsible_pid(server_pid).expect("应能读取 tmux server responsible pid");
        let _ = std::process::Command::new(tmux)
            .args(["-S", sock.to_str().expect("sock utf8"), "kill-server"])
            .status();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            responsible,
            server_pid,
            "tmux server 必须对自己负责，实际 responsible={responsible} caller={}",
            std::process::id()
        );
    }
}
