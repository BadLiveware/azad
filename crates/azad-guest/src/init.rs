//! The init of Azad's microVM sandbox. As PID 1 in the initramfs, it mounts the system file systems, tells the host
//! that it is ready, and starts a worker for each run that the host asks for: this program again, with the argument
//! `run`. When the host closes the control connection, it powers the VM off.
//!
//! A worker has its own mount namespace. It mounts the run's disks, builds the same view of the file system as the
//! bubblewrap sandbox, runs the command in a cgroup of its own, sends the output, exit code and work directory to the
//! host, and ends every process that the command left.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use azad_guest::protocol::{
    CONTROL_PORT, MOUNTS_DIR, Message, RUN_PORT, RunRequest, RunSpec, SPEC_FILE, TMP_DIR, VOLUME_DATA_DIR, WORK_DIR,
    WorkWriter, read_message, write_message,
};
use nix::errno::Errno;
use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};
use nix::sys::prctl::set_child_subreaper;
use nix::sys::reboot::{RebootMode, reboot};
use nix::sys::resource::{Resource, setrlimit};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::{Pid, chroot, fchdir, sync};

use crate::sys;

const INIT: &str = "/init";
const RUN_ARGUMENT: &str = "run";
const RUNS_DIR: &str = "/runs";
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const SYS_BLOCK: &str = "/sys/block";
const SANDBOX_WORK: &str = "/work";
const ETC: &str = "etc";
const RESOLV_CONF: &str = "resolv.conf";
/// Root entries that the sandbox makes itself, as in the bubblewrap sandbox.
const RESERVED_ROOT_ENTRIES: [&str; 4] = ["proc", "dev", "tmp", "work"];
/// The devices of `/dev`, as bubblewrap's `--dev` gives them.
const DEVICE_NODES: [&str; 6] = ["null", "zero", "full", "random", "urandom", "tty"];
const DEVICE_LINKS: [(&str, &str); 4] = [
    ("fd", "/proc/self/fd"),
    ("stdin", "/proc/self/fd/0"),
    ("stdout", "/proc/self/fd/1"),
    ("stderr", "/proc/self/fd/2"),
];
const DEV_MODE: &str = "mode=755";
const SHM_MODE: &str = "mode=1777";
const DEVICE_WAIT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const REAP_INTERVAL: Duration = Duration::from_millis(100);
const SIGNAL_EXIT_BASE: i32 = 128;
const DIR_MODE: u32 = 0o755;
/// The memory that the runs of a VM leave to the guest itself, so that a run that uses too much is ended alone.
const MEMORY_RESERVE: u64 = 64 << 20;
const KIBIBYTE: u64 = 1024;
const MEMORY_CONTROLLER: &str = "+memory";
const NOT_POPULATED: &str = "populated 0";

type HostChannel = Mutex<File>;

pub(crate) fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(RUN_ARGUMENT) => worker(),
        _ => init(),
    }
}

fn init() {
    if let Err(reason) = serve() {
        eprintln!("azad-guest: {reason}");
    }
    sync();
    let _ = reboot(RebootMode::RB_POWER_OFF);
}

fn serve() -> Result<(), String> {
    mount_system()?;
    let listener = sys::listen(RUN_PORT)?;
    let mut control = sys::connect_to_host(CONTROL_PORT)?;
    write_message(&mut control, &Message::Ready)
        .map_err(|e| format!("telling the host that the guest is ready: {e}"))?;
    thread::spawn(reap_children);
    thread::spawn(move || start_workers(&listener));
    // The host sends nothing more on the control connection; it closes it to power the VM off.
    let _ = control.read(&mut [0u8; 1]);
    Ok(())
}

fn mount_system() -> Result<(), String> {
    mount_fs("proc", "/proc", "proc", MsFlags::empty(), None)?;
    mount_fs("sysfs", "/sys", "sysfs", MsFlags::empty(), None)?;
    mount_fs("devtmpfs", "/dev", "devtmpfs", MsFlags::empty(), None)?;
    mount_fs("cgroup2", CGROUP_ROOT, "cgroup2", MsFlags::empty(), None)?;
    write_file(&Path::new(CGROUP_ROOT).join("cgroup.subtree_control"), MEMORY_CONTROLLER)?;
    sys::bring_up_loopback()
}

fn start_workers(listener: &OwnedFd) {
    loop {
        if let Err(reason) = sys::accept(listener).and_then(start_worker) {
            eprintln!("azad-guest: {reason}");
        }
    }
}

fn start_worker(connection: OwnedFd) -> Result<(), String> {
    let input = connection.try_clone().map_err(|e| format!("a copy of the run's connection: {e}"))?;
    Command::new(INIT)
        .arg(RUN_ARGUMENT)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(connection))
        .spawn()
        .map_err(|e| format!("starting a worker: {e}"))?;
    Ok(())
}

/// As PID 1, the init gets every process whose parent ended, and must collect each when it ends.
fn reap_children() {
    loop {
        if let Err(Errno::ECHILD) = waitpid(Pid::from_raw(-1), None) {
            thread::sleep(REAP_INTERVAL);
        }
    }
}

fn worker() {
    let (input, output) = match connection() {
        Ok(channels) => channels,
        Err(reason) => return eprintln!("azad-guest run: {reason}"),
    };
    let host = Mutex::new(output);
    if let Err(reason) = serve_run(input, &host) {
        send(&host, &Message::Failed(reason)).ok();
    }
    sync();
}

/// The run's connection, which the init gave the worker as its standard input and output.
fn connection() -> Result<(File, File), String> {
    let input = std::io::stdin().as_fd().try_clone_to_owned().map_err(|e| format!("the run's connection: {e}"))?;
    let output = std::io::stdout().as_fd().try_clone_to_owned().map_err(|e| format!("the run's connection: {e}"))?;
    Ok((File::from(input), File::from(output)))
}

/// Where a worker mounts the disks of its run and builds the command's root, in its own mount namespace.
struct RunDirs {
    name: String,
    root_image: PathBuf,
    run_disk: PathBuf,
    volumes: PathBuf,
    new_root: PathBuf,
}

impl RunDirs {
    fn new() -> Self {
        let name = format!("run-{}", std::process::id());
        let base = Path::new(RUNS_DIR).join(&name);
        RunDirs {
            root_image: base.join("root"),
            run_disk: base.join("disk"),
            volumes: base.join("volumes"),
            new_root: base.join("new"),
            name,
        }
    }

    /// Where the disk of a volume is mounted.
    fn volume_disk(&self, index: usize) -> PathBuf {
        self.volumes.join(index.to_string())
    }

    /// The volume's files, in a directory of its disk, without the disk's `lost+found`.
    fn volume(&self, index: usize) -> PathBuf {
        self.volume_disk(index).join(VOLUME_DATA_DIR)
    }
}

fn serve_run(mut input: File, host: &HostChannel) -> Result<(), String> {
    let request = match read_message(&mut input) {
        Ok(Some(Message::Run(request))) => request,
        Ok(other) => return Err(format!("the host sent {other:?} instead of a run")),
        Err(e) => return Err(format!("reading the run: {e}")),
    };
    unshare(CloneFlags::CLONE_NEWNS).map_err(|e| format!("a mount namespace for the run: {e}"))?;
    mount(None::<&str>, "/", None::<&str>, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None::<&str>)
        .map_err(|e| format!("making the run's mounts private: {e}"))?;
    set_child_subreaper(true).map_err(|e| format!("collecting the command's processes: {e}"))?;
    let dirs = RunDirs::new();
    mount_disks(&request, &dirs)?;
    let spec_path = dirs.run_disk.join(SPEC_FILE);
    let spec: RunSpec = serde_json::from_slice(&fs::read(&spec_path).map_err(|e| io_error(&spec_path, e))?)
        .map_err(|e| format!("{}: {e}", spec_path.display()))?;
    build_root(&spec, &dirs)?;
    let cgroup = Arc::new(Cgroup::create(&dirs.name)?);
    let watched = cgroup.clone();
    thread::spawn(move || end_when_host_leaves(input, &watched));
    let real_root = File::open("/").map_err(|e| format!("opening /: {e}"))?;
    chroot(&dirs.new_root).map_err(|e| format!("chroot to {}: {e}", dirs.new_root.display()))?;
    std::env::set_current_dir("/").map_err(|e| format!("cd /: {e}"))?;
    let ran = run_command(host, &spec, &cgroup);
    fchdir(&real_root).and_then(|()| chroot(".")).map_err(|e| format!("leaving the command's root: {e}"))?;
    cgroup.remove()?;
    ran
}

/// The host closes the run's connection when the run must end, for example at its time limit.
fn end_when_host_leaves(mut input: File, cgroup: &Cgroup) {
    let _ = input.read(&mut [0u8; 1]);
    cgroup.kill().ok();
}

/// The root image is mapped with DAX, so the guest reads it from the host's page cache without a copy; a kernel
/// that cannot do DAX on it mounts it without.
fn mount_disks(request: &RunRequest, dirs: &RunDirs) -> Result<(), String> {
    let root = device_at(&request.root)?;
    create_dir(&dirs.root_image)?;
    let read_only = MsFlags::MS_RDONLY;
    if mount_device(&root, &dirs.root_image, "erofs", read_only, Some("dax=always")).is_err() {
        mount_device(&root, &dirs.root_image, "erofs", read_only, None)?;
    }
    let run = device_at(&request.run)?;
    create_dir(&dirs.run_disk)?;
    mount_device(&run, &dirs.run_disk, "ext4", MsFlags::empty(), None)?;
    for (index, address) in request.volumes.iter().enumerate() {
        let device = device_at(address)?;
        create_dir(&dirs.volume_disk(index))?;
        mount_device(&device, &dirs.volume_disk(index), "ext4", MsFlags::empty(), None)?;
    }
    Ok(())
}

/// Mounts a hot-plugged device, which can still be unready for a moment after it has a size.
fn mount_device(device: &str, target: &Path, fstype: &str, flags: MsFlags, data: Option<&str>) -> Result<(), String> {
    let deadline = Instant::now() + DEVICE_WAIT;
    loop {
        match mount(Some(device), target, Some(fstype), flags, data) {
            Ok(()) => return Ok(()),
            Err(Errno::ENXIO) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Err(e) => return Err(format!("mounting {device} ({fstype}) at {}: {e}", target.display())),
        }
    }
}

/// The block device that the host attached at a PCI address. A hot-plugged device appears a moment after the host
/// attached it, and has a size when it is ready.
fn device_at(address: &str) -> Result<String, String> {
    let deadline = Instant::now() + DEVICE_WAIT;
    loop {
        if let Some(name) = block_device_at(address).filter(|name| is_ready(name)) {
            return Ok(format!("/dev/{name}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("no block device appeared at PCI address {address}"));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn block_device_at(address: &str) -> Option<String> {
    fs::read_dir(SYS_BLOCK)
        .ok()?
        .filter_map(Result::ok)
        .find(|entry| {
            fs::canonicalize(entry.path()).is_ok_and(|real| real.components().any(|part| part.as_os_str() == address))
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
}

fn is_ready(name: &str) -> bool {
    let size = fs::read_to_string(Path::new(SYS_BLOCK).join(name).join("size"));
    Path::new("/dev").join(name).exists() && size.is_ok_and(|size| size.trim() != "0")
}

/// The cgroup of one run: the command and every process that it starts are in it, so that the worker can end them
/// all, and a run that uses too much memory is ended alone. The files are opened before the worker changes its root,
/// which hides them.
struct Cgroup {
    dir: PathBuf,
    procs: File,
    parent_procs: File,
    kill: File,
    events: PathBuf,
}

impl Cgroup {
    fn create(name: &str) -> Result<Self, String> {
        let dir = Path::new(CGROUP_ROOT).join(name);
        fs::create_dir(&dir).map_err(|e| io_error(&dir, e))?;
        write_file(&dir.join("memory.max"), &memory_for_runs()?.to_string())?;
        write_file(&dir.join("memory.oom.group"), "1")?;
        let open = |path: PathBuf| File::options().write(true).open(&path).map_err(|e| io_error(&path, e));
        Ok(Cgroup {
            procs: open(dir.join("cgroup.procs"))?,
            parent_procs: open(Path::new(CGROUP_ROOT).join("cgroup.procs"))?,
            kill: open(dir.join("cgroup.kill"))?,
            events: dir.join("cgroup.events"),
            dir,
        })
    }

    /// Moves the worker in, so that the command starts in the cgroup; the worker leaves right after the start.
    fn enter(&self) -> Result<(), String> {
        write_pid(&self.procs).map_err(|e| format!("entering the run's cgroup: {e}"))
    }

    fn leave(&self) -> Result<(), String> {
        write_pid(&self.parent_procs).map_err(|e| format!("leaving the run's cgroup: {e}"))
    }

    fn kill(&self) -> Result<(), String> {
        (&self.kill).write_all(b"1").map_err(|e| format!("ending the run's processes: {e}"))
    }

    /// Ends what is left in the cgroup, collects it, and removes the cgroup.
    fn remove(&self) -> Result<(), String> {
        self.kill()?;
        let deadline = Instant::now() + DEVICE_WAIT;
        while !fs::read_to_string(&self.events).is_ok_and(|events| events.lines().any(|l| l == NOT_POPULATED)) {
            reap_ended();
            if Instant::now() >= deadline {
                return Err("the run's processes did not end".to_owned());
            }
            thread::sleep(POLL_INTERVAL);
        }
        reap_ended();
        fs::remove_dir(&self.dir).map_err(|e| io_error(&self.dir, e))
    }
}

fn write_pid(mut procs: &File) -> std::io::Result<()> {
    procs.write_all(std::process::id().to_string().as_bytes())
}

/// The worker is the subreaper of the command, so it gets the processes that the command left when they end.
fn reap_ended() {
    while waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)).is_ok_and(|status| status.pid().is_some()) {}
}

/// The memory of the VM, without what the guest keeps for itself.
fn memory_for_runs() -> Result<u64, String> {
    let meminfo = fs::read_to_string("/proc/meminfo").map_err(|e| format!("/proc/meminfo: {e}"))?;
    let total_kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .ok_or("/proc/meminfo has no MemTotal")?;
    Ok((total_kib * KIBIBYTE).saturating_sub(MEMORY_RESERVE))
}

fn run_command(host: &HostChannel, spec: &RunSpec, cgroup: &Cgroup) -> Result<(), String> {
    set_limits(spec)?;
    let start = start_dir(&spec.dir);
    if !start.is_dir() {
        return Err(format!("the start directory {} does not exist", start.display()));
    }
    let Some((program, args)) = spec.command.split_first() else {
        return Err("the action has no command".to_owned());
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(&spec.env)
        .current_dir(&start)
        .uid(spec.uid)
        .gid(spec.gid)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cgroup.enter()?;
    let spawned = command.spawn();
    cgroup.leave()?;
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return send(host, &Message::NotFound),
        Err(e) => return Err(format!("starting {program}: {e}")),
    };
    let stdout = child.stdout.take().ok_or("the standard output is not piped")?;
    let stderr = child.stderr.take().ok_or("the standard error is not piped")?;
    let status = thread::scope(|scope| {
        let out = scope.spawn(|| forward_lines(stdout, host, Message::Stdout));
        let err = scope.spawn(|| forward_lines(stderr, host, Message::Stderr));
        let status = child.wait().map_err(|e| format!("waiting for the command: {e}"));
        // Processes that the command left can hold its output open; they end with it, as in the bubblewrap sandbox.
        cgroup.kill()?;
        for copy in [out, err] {
            copy.join().map_err(|_| "a copy of the output panicked")??;
        }
        status
    })?;
    let code = status.code().unwrap_or_else(|| SIGNAL_EXIT_BASE + status.signal().unwrap_or_default());
    send(host, &Message::Exit(code))?;
    send_work(host)
}

fn start_dir(dir: &str) -> PathBuf {
    match dir.is_empty() {
        true => PathBuf::from(SANDBOX_WORK),
        false => Path::new(SANDBOX_WORK).join(dir),
    }
}

/// The limits apply to the command, which inherits them; the worker is root, which the process limit does not bind.
fn set_limits(spec: &RunSpec) -> Result<(), String> {
    let limits = [(Resource::RLIMIT_AS, spec.memory_bytes), (Resource::RLIMIT_NPROC, spec.processes)];
    for (resource, limit) in limits {
        if let Some(limit) = limit {
            setrlimit(resource, limit, limit).map_err(|e| format!("the limit {resource:?}: {e}"))?;
        }
    }
    Ok(())
}

fn forward_lines(pipe: impl Read, host: &HostChannel, message: fn(Vec<u8>) -> Message) -> Result<(), String> {
    let mut reader = BufReader::new(pipe);
    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line).map_err(|e| format!("reading the output: {e}"))?;
        if n == 0 {
            return Ok(());
        }
        send(host, &message(line))?;
    }
}

fn send(host: &HostChannel, message: &Message) -> Result<(), String> {
    let mut channel = host.lock().map_err(|_| "the host channel lock is poisoned")?;
    write_message(&mut *channel, message).map_err(|e| format!("sending to the host: {e}"))
}

fn send_work(host: &HostChannel) -> Result<(), String> {
    let mut channel = host.lock().map_err(|_| "the host channel lock is poisoned")?;
    let mut archive = tar::Builder::new(WorkWriter::new(&mut *channel));
    archive.follow_symlinks(false);
    archive.append_dir_all(".", SANDBOX_WORK).map_err(|e| format!("packing {SANDBOX_WORK}: {e}"))?;
    let writer = archive.into_inner().map_err(|e| format!("packing {SANDBOX_WORK}: {e}"))?;
    writer.finish().map_err(|e| format!("sending {SANDBOX_WORK}: {e}"))?;
    Ok(())
}

/// The root of the command, in the order of the bubblewrap sandbox: the image's entries read-only on a tmpfs, then
/// `/etc/resolv.conf` for an action with network, `/proc`, `/dev`, `/tmp`, the read-only mounts, the cache volumes
/// outside `/work`, `/work`, and the cache volumes in it; then the tmpfs becomes read-only.
fn build_root(spec: &RunSpec, dirs: &RunDirs) -> Result<(), String> {
    let new_root = dirs.new_root.as_path();
    create_dir(new_root)?;
    mount_fs("tmpfs", new_root, "tmpfs", MsFlags::empty(), None)?;
    let mut reserved = RESERVED_ROOT_ENTRIES.to_vec();
    if spec.resolv_conf.is_some() {
        reserved.push(ETC);
    }
    bind_entries(&dirs.root_image, new_root, &reserved)?;
    if let Some(resolv_conf) = &spec.resolv_conf {
        add_resolv_conf(&dirs.root_image.join(ETC), &new_root.join(ETC), resolv_conf)?;
    }
    let proc = new_root.join("proc");
    create_dir(&proc)?;
    mount_fs("proc", &proc, "proc", MsFlags::empty(), None)?;
    make_dev(&new_root.join("dev"))?;
    let run = dirs.run_disk.as_path();
    bind(&run.join(TMP_DIR), &new_root.join("tmp"), Access::Write)?;
    let mut mounts: Vec<(usize, &String)> = spec.mounts.iter().enumerate().collect();
    mounts.sort_by(|a, b| a.1.cmp(b.1));
    for (index, target) in mounts {
        let source = run.join(MOUNTS_DIR).join(index.to_string());
        bind(&source, &in_root(new_root, target), Access::Read)?;
    }
    let mut volumes: Vec<(usize, &String)> = spec.volumes.iter().enumerate().collect();
    volumes.sort_by(|a, b| a.1.cmp(b.1));
    let (in_work, elsewhere): (Vec<_>, Vec<_>) =
        volumes.into_iter().partition(|(_, target)| target.starts_with(&format!("{SANDBOX_WORK}/")));
    for (index, target) in elsewhere {
        bind(&dirs.volume(index), &in_root(new_root, target), Access::Write)?;
    }
    bind(&run.join(WORK_DIR), &new_root.join(WORK_DIR), Access::Write)?;
    for (index, target) in in_work {
        bind(&dirs.volume(index), &in_root(new_root, target), Access::Write)?;
    }
    mount(None::<&str>, new_root, None::<&str>, MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, None::<&str>)
        .map_err(|e| format!("making {} read-only: {e}", new_root.display()))
}

fn in_root(root: &Path, target: &str) -> PathBuf {
    root.join(target.trim_start_matches('/'))
}

/// Images do not ship `/etc/resolv.conf`; as in the bubblewrap sandbox, `/etc` holds the image's entries and this
/// one file.
fn add_resolv_conf(image_etc: &Path, etc: &Path, content: &str) -> Result<(), String> {
    create_dir(etc)?;
    if image_etc.is_dir() {
        bind_entries(image_etc, etc, &[RESOLV_CONF])?;
    }
    write_file(&etc.join(RESOLV_CONF), content)
}

/// Shows each entry of `source` read-only in `target`, except the `reserved` names; symbolic links are copied.
fn bind_entries(source: &Path, target: &Path, reserved: &[&str]) -> Result<(), String> {
    let mut names: Vec<String> = fs::read_dir(source)
        .map_err(|e| io_error(source, e))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| io_error(source, e))?;
    names.sort();
    for name in names.iter().filter(|name| !reserved.contains(&name.as_str())) {
        let (from, to) = (source.join(name), target.join(name));
        let metadata = fs::symlink_metadata(&from).map_err(|e| io_error(&from, e))?;
        if metadata.is_symlink() {
            let link = fs::read_link(&from).map_err(|e| io_error(&from, e))?;
            symlink(&link, &to).map_err(|e| io_error(&to, e))?;
            continue;
        }
        bind(&from, &to, Access::Read)?;
    }
    Ok(())
}

fn make_dev(dev: &Path) -> Result<(), String> {
    create_dir(dev)?;
    mount_fs("tmpfs", dev, "tmpfs", MsFlags::MS_NOSUID, Some(DEV_MODE))?;
    for node in DEVICE_NODES {
        bind(&Path::new("/dev").join(node), &dev.join(node), Access::Write)?;
    }
    for (name, target) in DEVICE_LINKS {
        symlink(target, dev.join(name)).map_err(|e| io_error(&dev.join(name), e))?;
    }
    let shm = dev.join("shm");
    create_dir(&shm)?;
    mount_fs("tmpfs", &shm, "tmpfs", MsFlags::MS_NOSUID | MsFlags::MS_NODEV, Some(SHM_MODE))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
}

/// Binds `source` at `target`, and makes the mount point first: a directory or an empty file, as the source is.
fn bind(source: &Path, target: &Path, access: Access) -> Result<(), String> {
    let is_dir = fs::metadata(source).map_err(|e| io_error(source, e))?.is_dir();
    match is_dir {
        true => fs::create_dir_all(target).map_err(|e| io_error(target, e))?,
        false => {
            File::create(target).map_err(|e| io_error(target, e))?;
        }
    }
    let flags = MsFlags::MS_BIND | MsFlags::MS_REC;
    mount(Some(source), target, None::<&str>, flags, None::<&str>)
        .map_err(|e| format!("binding {} at {}: {e}", source.display(), target.display()))?;
    if access == Access::Write {
        return Ok(());
    }
    let read_only = MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY;
    mount(None::<&str>, target, None::<&str>, read_only, None::<&str>)
        .map_err(|e| format!("making {} read-only: {e}", target.display()))
}

fn mount_fs(
    source: &str,
    target: impl AsRef<Path>,
    fstype: &str,
    flags: MsFlags,
    data: Option<&str>,
) -> Result<(), String> {
    let target = target.as_ref();
    mount(Some(source), target, Some(fstype), flags, data)
        .map_err(|e| format!("mounting {source} ({fstype}) at {}: {e}", target.display()))
}

fn create_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE)).map_err(|e| io_error(dir, e))
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|e| io_error(path, e))
}

fn io_error(path: &Path, error: std::io::Error) -> String {
    format!("{}: {error}", path.display())
}
