//! One VM that serves the runs of a sandbox group: it boots once, and the disks of each run are attached while the
//! run lasts. Several runs can share the VM at the same time; each has its own mount namespace and cgroup in the guest.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle, ScopedJoinHandle};
use std::time::{Duration, Instant};

use azad_guest::protocol::{CONTROL_PORT, Message, RUN_PORT, RunRequest, read_message, write_message};
use tempfile::TempDir;

use crate::log::{LogSink, Stream};
use crate::sandbox::process::OutputFiles;
use crate::sandbox::vm::api::Api;
use crate::sandbox::vm::network::GuestNetwork;
use crate::sandbox::vm::{VmConfig, disks, initramfs, io_error};
use crate::sandbox::{Exit, PATH_VARIABLE, SandboxError, SandboxSpec};
use crate::timings::Timings;

const VSOCK_SOCKET: &str = "vsock.sock";
const API_SOCKET: &str = "api.sock";
const EVENTS_FILE: &str = "events.json";
const CONSOLE_FILE: &str = "console.log";
const WORK_TAR: &str = "work.tar";
const GUEST_CID: u32 = 3;
const KERNEL_COMMAND_LINE: &str = "console=ttyS0 panic=1 quiet";
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long the guest may take from the start of the VMM to its connection to the host.
const BOOT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the VMM may take to end after the host closed the control connection.
const POWER_OFF_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the guest may take to let go of a run's disks after it sent everything.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(15);
const CONSOLE_TAIL_BYTES: usize = 2048;
/// The PCI slots that the runs of one VM may use at the same time; the bus has 32, and the VM's own devices use some.
const DEVICE_SLOTS: usize = 24;
/// Each run has a root image and a run disk, besides its cache volumes.
const DEVICES_PER_RUN: usize = 2;
const CONNECTED: &str = "OK ";
const GROUP_DIR_PREFIX: &str = "vm-";

pub(super) struct VmGroup {
    config: VmConfig,
    network: Option<GuestNetwork>,
    dir: TempDir,
    vmm: Arc<Vmm>,
    api: Api,
    control: UnixStream,
    watcher: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    /// Set when the guest kept a run's disks, so that it would run out of PCI slots: the VM takes no more runs, and
    /// stops when its last run ends.
    retired: AtomicBool,
    next_device: AtomicU64,
    slots: Slots,
    timings: Timings,
}

impl std::fmt::Debug for VmGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmGroup").field("dir", &self.dir.path()).field("network", &self.network.is_some()).finish()
    }
}

impl VmGroup {
    /// Boots a VM, and waits until its guest is ready for runs. With `network`, the VM gets a network device behind
    /// the rules of [`GuestNetwork`].
    pub(super) fn start(config: &VmConfig, network: bool, timings: Timings) -> Result<Self, SandboxError> {
        fs::create_dir_all(&config.images).map_err(|e| io_error(&config.images, e))?;
        let dir = TempDir::with_prefix_in(GROUP_DIR_PREFIX, &config.images).map_err(|e| io_error(&config.images, e))?;
        let network = match network {
            true => Some(GuestNetwork::start(dir.path())?),
            false => None,
        };
        let initramfs = initramfs::initramfs(&config.images, &config.guest)?;
        let control_path = dir.path().join(format!("{VSOCK_SOCKET}_{CONTROL_PORT}"));
        let listener = UnixListener::bind(&control_path).map_err(|e| io_error(&control_path, e))?;
        listener.set_nonblocking(true).map_err(|e| io_error(&control_path, e))?;
        let console_path = dir.path().join(CONSOLE_FILE);
        let console = File::create(&console_path).map_err(|e| io_error(&console_path, e))?;
        let console_err = console.try_clone().map_err(|e| io_error(&console_path, e))?;
        let mut command = match &network {
            Some(network) => network.command(&config.vmm),
            None => Command::new(&config.vmm),
        };
        let timer = timings.start("boot a VM");
        let child = command
            .args(arguments(config, &initramfs, dir.path(), network.as_ref()))
            .stdin(Stdio::null())
            .stdout(console)
            .stderr(console_err)
            .spawn()
            .map_err(SandboxError::Spawn)?;
        let vmm =
            Arc::new(Vmm { child: Mutex::new(child), events: dir.path().join(EVENTS_FILE), ended: Mutex::default() });
        let control = match wait_for_guest(&listener, &vmm) {
            Ok(control) => control,
            Err(reason) => {
                vmm.kill();
                return Err(SandboxError::Setup(format!("{reason}; {}", tail(&console_path))));
            }
        };
        timer.stop();
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = Some(thread::spawn({
            let (vmm, stop) = (vmm.clone(), stop.clone());
            move || vmm.watch(&stop)
        }));
        Ok(VmGroup {
            config: config.clone(),
            network,
            api: Api::new(dir.path().join(API_SOCKET)),
            dir,
            vmm,
            control,
            watcher,
            stop,
            retired: AtomicBool::new(false),
            next_device: AtomicU64::new(0),
            slots: Slots::new(DEVICE_SLOTS),
            timings,
        })
    }

    /// Why the VM no longer runs, when it does not.
    /// Whether the VM takes new runs: it runs, and no run found it unwell.
    pub(super) fn takes_runs(&self) -> bool {
        self.ended().is_none() && !self.retired.load(Ordering::SeqCst)
    }

    pub(super) fn ended(&self) -> Option<String> {
        self.vmm.ended.lock().ok()?.clone()
    }

    pub(super) fn run(&self, spec: &SandboxSpec, logs: &dyn LogSink) -> Result<Exit, SandboxError> {
        if let Some(reason) = self.ended() {
            return Err(self.failure(&reason));
        }
        let parent = spec.work.parent().unwrap_or(&spec.work);
        let dir = TempDir::new_in(parent).map_err(|e| io_error(parent, e))?;
        let timer = self.timings.start("make the VM's image of the root");
        let root = disks::root_image(&self.config.images, &spec.root)?;
        timer.stop();
        let timer = self.timings.start("make the VM's run disk");
        let resolv_conf = self.network.as_ref().map(|_| GuestNetwork::resolv_conf());
        let disk = disks::run_disk(spec, dir.path(), self.config.disk_room, resolv_conf)?;
        timer.stop();
        let volumes = spec
            .volumes
            .iter()
            .map(|volume| disks::volume_disk(&volume.disk, self.config.volume_size).map(|()| volume.disk.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let _slots = self.slots.take(DEVICES_PER_RUN + volumes.len());
        let mut attached = Attached { api: &self.api, ids: Vec::new() };
        let request = RunRequest {
            root: attached.pmem(&root, self.device_id("root"))?,
            run: attached.disk(&disk, self.device_id("run"))?,
            volumes: volumes
                .iter()
                .map(|volume| attached.disk(volume, self.device_id("volume")))
                .collect::<Result<_, _>>()?,
        };
        let stream = self.connect_to_guest()?;
        write_message(&mut &stream, &Message::Run(request)).map_err(SandboxError::Spawn)?;
        let output = OutputFiles::create(&spec.stdout, &spec.stderr, &spec.log)?;
        let work_tar = dir.path().join(WORK_TAR);
        let deadline = Instant::now() + spec.limits.timeout;
        let (received, timed_out) = thread::scope(|scope| {
            let reader = scope.spawn(|| receive(&stream, output, &work_tar, logs));
            let timed_out = self.wait(&reader, &stream, deadline);
            (reader.join(), timed_out)
        });
        let received = received.map_err(|_| SandboxError::Setup("the VM reader panicked".to_owned()))?;
        if attached.detach_all().is_err() {
            self.retired.store(true, Ordering::SeqCst);
        }
        if timed_out {
            return Ok(Exit::TimedOut);
        }
        match received? {
            Received::Exit(code) => {
                let timer = self.timings.start("copy the VM's work directory back");
                replace_work(&spec.work, &work_tar)?;
                timer.stop();
                Ok(Exit::Code(code))
            }
            Received::NotFound => Err(SandboxError::CommandNotFound {
                command: spec.command.first().cloned().unwrap_or_default(),
                path: spec.env.get(PATH_VARIABLE).cloned(),
            }),
            Received::Failed(reason) => Err(SandboxError::Setup(reason)),
            Received::Nothing => {
                let reason = self.ended().unwrap_or_else(|| "the guest ended the run without a result".to_owned());
                Err(self.failure(&reason))
            }
        }
    }

    /// Waits until the reader is done. At the deadline, the host closes its side of the run's connection, and the
    /// guest ends the command and lets go of the disks; a guest that does not, or a VM that ended, gets the
    /// connection closed. Returns whether the run reached its deadline.
    fn wait(
        &self,
        reader: &ScopedJoinHandle<'_, Result<Received, SandboxError>>,
        stream: &UnixStream,
        deadline: Instant,
    ) -> bool {
        let mut timed_out = false;
        while !reader.is_finished() {
            let now = Instant::now();
            if now >= deadline && !timed_out {
                stream.shutdown(Shutdown::Write).ok();
                timed_out = true;
            }
            if self.ended().is_some() || now >= deadline + RELEASE_TIMEOUT {
                stream.shutdown(Shutdown::Both).ok();
            }
            thread::sleep(POLL_INTERVAL);
        }
        timed_out
    }

    fn device_id(&self, kind: &str) -> String {
        format!("{kind}{}", self.next_device.fetch_add(1, Ordering::SeqCst))
    }

    /// Cloud Hypervisor connects the host to a port of the guest after `CONNECT <port>` on its vsock socket.
    fn connect_to_guest(&self) -> Result<UnixStream, SandboxError> {
        let path = self.dir.path().join(VSOCK_SOCKET);
        let mut stream = UnixStream::connect(&path).map_err(|e| io_error(&path, e))?;
        stream.write_all(format!("CONNECT {RUN_PORT}\n").as_bytes()).map_err(|e| io_error(&path, e))?;
        let mut answer = Vec::new();
        let mut byte = [0u8; 1];
        while byte[0] != b'\n' {
            match stream.read(&mut byte).map_err(|e| io_error(&path, e))? {
                0 => return Err(SandboxError::Setup("the guest refused a run's connection".to_owned())),
                _ => answer.push(byte[0]),
            }
        }
        match String::from_utf8_lossy(&answer).starts_with(CONNECTED) {
            true => Ok(stream),
            false => Err(SandboxError::Setup(format!("the VMM answered {:?} to a run's connection", answer))),
        }
    }

    fn failure(&self, reason: &str) -> SandboxError {
        SandboxError::Setup(format!("{reason}; {}", tail(&self.dir.path().join(CONSOLE_FILE))))
    }
}

/// Closing the control connection tells the guest to power off; a VMM that does not end soon is killed.
impl Drop for VmGroup {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(watcher) = self.watcher.take() {
            watcher.join().ok();
        }
        self.control.shutdown(Shutdown::Both).ok();
        let deadline = Instant::now() + POWER_OFF_TIMEOUT;
        while !self.vmm.exited() && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        self.vmm.kill();
    }
}

fn arguments(config: &VmConfig, initramfs: &Path, dir: &Path, network: Option<&GuestNetwork>) -> Vec<String> {
    let path = |p: &Path| p.display().to_string();
    let command_line = match network {
        Some(network) => format!("{KERNEL_COMMAND_LINE} {}", network.kernel_argument()),
        None => KERNEL_COMMAND_LINE.to_owned(),
    };
    let mut args = vec![
        "--kernel".to_owned(),
        path(&config.kernel),
        "--initramfs".to_owned(),
        path(initramfs),
        "--cmdline".to_owned(),
        command_line,
        "--cpus".to_owned(),
        format!("boot={}", config.cpus),
        "--memory".to_owned(),
        format!("size={}M", config.memory_mib),
        "--vsock".to_owned(),
        format!("cid={GUEST_CID},socket={}", path(&dir.join(VSOCK_SOCKET))),
        "--api-socket".to_owned(),
        format!("path={}", path(&dir.join(API_SOCKET))),
        "--serial".to_owned(),
        "tty".to_owned(),
        "--console".to_owned(),
        "off".to_owned(),
        "--event-monitor".to_owned(),
        format!("path={}", path(&dir.join(EVENTS_FILE))),
        "--pvpanic".to_owned(),
        "--landlock".to_owned(),
    ];
    // The VMM opens the disks of each run while the VM runs, so Landlock must allow their directories from the start.
    for allowed in std::iter::once(dir).chain(config.landlock.iter().map(PathBuf::as_path)) {
        args.extend(["--landlock-rules".to_owned(), format!("path={},access=rw", path(allowed))]);
    }
    if let Some(network) = network {
        args.extend(["--net".to_owned(), network.device_argument()]);
    }
    args
}

/// Takes the guest's control connection, which says that the guest is ready.
fn wait_for_guest(listener: &UnixListener, vmm: &Vmm) -> Result<UnixStream, String> {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return ready(stream),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("the guest's control connection: {e}")),
        }
        if let Some(reason) = vmm.check() {
            return Err(reason);
        }
        if Instant::now() >= deadline {
            return Err(format!("the guest did not boot in {}s", BOOT_TIMEOUT.as_secs()));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn ready(mut stream: UnixStream) -> Result<UnixStream, String> {
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    match read_message(&mut stream) {
        Ok(Some(Message::Ready)) => Ok(stream),
        Ok(other) => Err(format!("the guest sent {other:?} instead of ready")),
        Err(e) => Err(format!("the guest's control connection: {e}")),
    }
}

/// The devices that a run attached, which it detaches when it ends.
struct Attached<'a> {
    api: &'a Api,
    ids: Vec<String>,
}

impl Attached<'_> {
    fn pmem(&mut self, file: &Path, id: String) -> Result<String, SandboxError> {
        let address = self.api.add_pmem(file, &id)?;
        self.ids.push(id);
        Ok(address)
    }

    fn disk(&mut self, file: &Path, id: String) -> Result<String, SandboxError> {
        let address = self.api.add_disk(file, &id)?;
        self.ids.push(id);
        Ok(address)
    }

    /// Detaches every device, and waits until the VMM has let go of their files: the next run can attach the disk of
    /// the same cache volume.
    fn detach_all(&mut self) -> Result<(), SandboxError> {
        let ids = std::mem::take(&mut self.ids);
        for id in &ids {
            self.api.remove_device(id)?;
        }
        let deadline = Instant::now() + RELEASE_TIMEOUT;
        for id in &ids {
            while self.api.has_device(id)? {
                if Instant::now() >= deadline {
                    return Err(SandboxError::Setup(format!("the guest did not let go of the device {id}")));
                }
                thread::sleep(POLL_INTERVAL);
            }
        }
        Ok(())
    }
}

impl Drop for Attached<'_> {
    fn drop(&mut self) {
        self.detach_all().ok();
    }
}

/// The PCI slots that are free for runs; a run waits until it can take the slots of its disks.
struct Slots {
    free: Mutex<usize>,
    freed: Condvar,
}

struct TakenSlots<'a> {
    slots: &'a Slots,
    count: usize,
}

impl Slots {
    fn new(count: usize) -> Self {
        Slots { free: Mutex::new(count), freed: Condvar::new() }
    }

    fn take(&self, count: usize) -> TakenSlots<'_> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free < count {
            free = self.freed.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= count;
        TakenSlots { slots: self, count }
    }
}

impl Drop for TakenSlots<'_> {
    fn drop(&mut self) {
        *self.slots.free.lock().unwrap_or_else(|e| e.into_inner()) += self.count;
        self.slots.freed.notify_all();
    }
}

enum Received {
    Exit(i32),
    NotFound,
    Failed(String),
    Nothing,
}

/// Copies what the guest sends for a run: output lines to their files and to `logs`, the work directory to
/// `work_tar`. After everything, it waits until the guest closes the connection, which the guest does when it no
/// longer uses the run's disks.
fn receive(
    stream: &UnixStream,
    mut output: OutputFiles,
    work_tar: &Path,
    logs: &dyn LogSink,
) -> Result<Received, SandboxError> {
    let mut reader = BufReader::new(stream);
    let mut received = Received::Nothing;
    let mut work = File::create(work_tar).map_err(|e| io_error(work_tar, e))?;
    while let Some(message) = read_message(&mut reader).map_err(SandboxError::Spawn)? {
        match message {
            Message::Stdout(line) => output.write_line(Stream::Stdout, &line, logs).map_err(SandboxError::Spawn)?,
            Message::Stderr(line) => output.write_line(Stream::Stderr, &line, logs).map_err(SandboxError::Spawn)?,
            Message::Exit(code) => received = Received::Exit(code),
            Message::NotFound => received = Received::NotFound,
            Message::Failed(reason) => received = Received::Failed(reason),
            Message::Work(part) => work.write_all(&part).map_err(|e| io_error(work_tar, e))?,
            Message::WorkEnd => break,
            Message::Ready | Message::Run(_) => {
                return Err(SandboxError::Setup("the guest sent a message of the host".to_owned()));
            }
        }
    }
    if matches!(received, Received::Exit(_)) && !wait_for_release(&mut reader) {
        return Err(SandboxError::Setup("the guest did not let go of the run's disks".to_owned()));
    }
    Ok(received)
}

/// Whether the guest closed the run's connection in time.
fn wait_for_release(reader: &mut BufReader<&UnixStream>) -> bool {
    if reader.get_ref().set_read_timeout(Some(RELEASE_TIMEOUT)).is_err() {
        return false;
    }
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).is_ok()
}

/// The VMM process, which a watcher checks while the VM runs.
struct Vmm {
    child: Mutex<Child>,
    events: PathBuf,
    ended: Mutex<Option<String>>,
}

impl Vmm {
    fn watch(&self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            if let Some(reason) = self.check() {
                self.end(reason);
                return;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Why the VM ended, if it did. A guest that reboots or panics is killed, because Cloud Hypervisor would boot it
    /// again.
    fn check(&self) -> Option<String> {
        if self.exited() {
            return Some("the VMM ended".to_owned());
        }
        let event = self.guest_ended()?;
        self.kill();
        Some(format!("the guest {event}"))
    }

    fn end(&self, reason: String) {
        self.kill();
        if let Ok(mut ended) = self.ended.lock() {
            ended.get_or_insert(reason);
        }
    }

    fn exited(&self) -> bool {
        let Ok(mut child) = self.child.lock() else { return true };
        !matches!(child.try_wait(), Ok(None))
    }

    fn kill(&self) {
        if let Ok(mut child) = self.child.lock()
            && matches!(child.try_wait(), Ok(None))
        {
            child.kill().ok();
            child.wait().ok();
        }
    }

    fn guest_ended(&self) -> Option<&'static str> {
        if self.has_event("vm", "rebooting") {
            return Some("rebooted");
        }
        self.has_event("guest", "panic").then_some("panicked")
    }

    /// Whether Cloud Hypervisor's event monitor reported an event, such as `vm`/`rebooting`.
    fn has_event(&self, source: &str, event: &str) -> bool {
        let Ok(text) = fs::read_to_string(&self.events) else { return false };
        serde_json::Deserializer::from_str(&text).into_iter::<serde_json::Value>().map_while(Result::ok).any(|value| {
            value.get("source").and_then(|s| s.as_str()) == Some(source)
                && value.get("event").and_then(|e| e.as_str()) == Some(event)
        })
    }
}

/// Puts the guest's work directory in place of the host's, so that the run's outputs are where the bubblewrap sandbox
/// leaves them.
fn replace_work(work: &Path, work_tar: &Path) -> Result<(), SandboxError> {
    for entry in fs::read_dir(work).map_err(|e| io_error(work, e))? {
        let path = entry.map_err(|e| io_error(work, e))?.path();
        let removed = match path.is_dir() && !path.is_symlink() {
            true => fs::remove_dir_all(&path),
            false => fs::remove_file(&path),
        };
        removed.map_err(|e| io_error(&path, e))?;
    }
    let file = File::open(work_tar).map_err(|e| io_error(work_tar, e))?;
    let mut archive = tar::Archive::new(file);
    archive.set_preserve_permissions(true);
    archive.unpack(work).map_err(|e| io_error(work, e))
}

fn tail(path: &Path) -> String {
    let text = fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
    let start = text.len().saturating_sub(CONSOLE_TAIL_BYTES);
    let start = (start..text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
    format!("the console ends with:\n{}", text[start..].trim())
}
