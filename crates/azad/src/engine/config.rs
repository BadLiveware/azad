use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use crate::frontend::LocalRun;
use crate::history::History;
use crate::sandbox::{Limits, VmConfig};
use crate::trust::Trust;

use crate::engine::EngineError;

const STORE_ENV: &str = "AZAD_STORE";
const CACHE_DIR_ENV: &str = "XDG_CACHE_HOME";
const HOME_ENV: &str = "HOME";
const STORE_DIR_NAME: &str = "azad";
const ROOT_MARKER: &str = ".git";

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The store directory.
    pub store: PathBuf,
    /// The workspace root, against which labels and globs resolve.
    pub root: PathBuf,
    /// The platform of the actions, such as `linux/amd64`.
    pub platform: String,
    pub limits: Limits,
    /// Files that frontends ask for by name, such as the GHA runner binary.
    pub tools: BTreeMap<String, PathBuf>,
    /// Whether tasks run in their images, as in CI, instead of on this host (decision 0015).
    pub isolated: bool,
    /// Whether commands are asked for colored output, because a terminal shows it.
    pub color: bool,
    /// Whether every action that a goal needs runs again, even when its result is known.
    pub force: bool,
    /// Where actions that do not run on the host run.
    pub sandbox: SandboxKind,
    /// The socket of an executor service that runs the actions, instead of this process (decision 0019).
    pub executor: Option<PathBuf>,
    /// The trust level of the runs, which chooses the action cache and the cache volumes that they write.
    pub trust: Trust,
    /// A source of the history that a shallow clone lacks, such as GitHub's API (decision 0021).
    pub history: Option<Arc<dyn History>>,
    /// Variables of this process that host actions do not get: Azad's own secrets, such as the key that signs cache
    /// entries.
    pub withheld: BTreeSet<String>,
    /// Which of the tasks and actions that a target selects run, by their tags (decision 0023).
    pub filter: TagFilter,
    /// Where a local run runs every task with an image, whatever the task says; `None` to follow each task (decision
    /// 0024).
    pub local: Option<LocalRun>,
}

/// Which of the tasks and actions that a target selects run, by their tags (decision 0023). It chooses only what a
/// target selects; what those need runs whatever its tags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagFilter {
    /// When not empty, only what has one of these tags runs.
    pub include: BTreeSet<String>,
    /// What has one of these tags does not run, even when it has an included tag too.
    pub exclude: BTreeSet<String>,
    /// Where the filter comes from, for notes, such as `--exclude` or a file of personal defaults.
    pub origin: String,
}

impl TagFilter {
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Whether something with `tags` runs.
    pub fn admits(&self, tags: &BTreeSet<String>) -> bool {
        if tags.iter().any(|tag| self.exclude.contains(tag)) {
            return false;
        }
        self.include.is_empty() || tags.iter().any(|tag| self.include.contains(tag))
    }
}

/// The sandboxes that the local backend can use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxKind {
    /// bubblewrap: namespaces on this host's kernel.
    #[default]
    Bubblewrap,
    /// microVMs: one for the actions without network and one for those with network, which their runs share
    /// (decision 0019).
    Vm,
}

impl EngineConfig {
    /// The defaults: the store from `store`, `$AZAD_STORE`, `$XDG_CACHE_HOME/azad` or `~/.cache/azad`; the root from
    /// `root`, or the nearest directory above the current one that holds `.git`, or the current directory.
    pub fn from_environment(store: Option<PathBuf>, root: Option<PathBuf>) -> Result<Self, EngineError> {
        let store = match store.or_else(|| env::var_os(STORE_ENV).map(PathBuf::from)) {
            Some(store) => store,
            None => default_store_dir()?,
        };
        let root = match root {
            Some(root) => root,
            None => default_root()?,
        };
        Ok(EngineConfig {
            store,
            root,
            platform: host_platform(),
            limits: Limits::default(),
            tools: default_tools(),
            isolated: false,
            color: false,
            force: false,
            sandbox: SandboxKind::default(),
            executor: None,
            trust: Trust::default(),
            history: None,
            withheld: BTreeSet::new(),
            filter: TagFilter::default(),
            local: None,
        })
    }
}

/// The name of the GHA runner tool.
pub const GHA_RUNNER: &str = "azad-gha-runner";
const GHA_RUNNER_ENV: &str = "AZAD_GHA_RUNNER";
/// The name of the init of the VM sandbox.
pub const GUEST: &str = "azad-guest";
const GUEST_ENV: &str = "AZAD_GUEST";
const RELEASE_PROFILE: &str = "release";
const DEBUG_PROFILE: &str = "debug";

/// The tools that can be found: the GHA runner from `$AZAD_GHA_RUNNER` and the VM's init from `$AZAD_GUEST`, or a
/// static build of each from the musl build directories of a Cargo build next to this program, or next to this program.
pub fn default_tools() -> BTreeMap<String, PathBuf> {
    [(GHA_RUNNER, GHA_RUNNER_ENV), (GUEST, GUEST_ENV)]
        .into_iter()
        .filter_map(|(name, variable)| Some((name.to_owned(), find_static_tool(name, variable)?)))
        .collect()
}

const VMM_ENV: &str = "AZAD_VMM";
const VMM_PROGRAM: &str = "cloud-hypervisor";
const KERNEL_ENV: &str = "AZAD_VM_KERNEL";
const VM_IMAGES_DIR: &str = "vm";
const VM_DISK_ROOM: u64 = 16 << 30;
const VM_VOLUME_SIZE: u64 = 64 << 30;
/// The memory of a VM. The guest kernel sets up each page at boot, so a larger VM boots slower: on a laptop, a goal
/// of three actions took 6.3 s with 8 GiB, 7.3 s with 16 GiB and 12.4 s with 60 GiB.
const VM_MEMORY_MIB: u64 = 8192;
const MEMINFO: &str = "/proc/meminfo";
const KIB_PER_MIB: u64 = 1024;

/// What the VM sandbox needs: Cloud Hypervisor from `$AZAD_VMM` or the `PATH`, the guest kernel from
/// `$AZAD_VM_KERNEL`, and the guest init; images are kept in the store. The runs of a VM share it, so it gets every
/// CPU of the host, and [`VM_MEMORY_MIB`] or half of the host's memory when that is less; the guest takes the host's
/// memory only as it uses it.
pub(crate) fn vm_config(config: &EngineConfig) -> Result<VmConfig, EngineError> {
    if !cfg!(target_os = "linux") {
        return Err(EngineError::Config("the VM sandbox needs Linux with KVM".to_owned()));
    }
    let kernel = env::var_os(KERNEL_ENV).filter(|path| !path.is_empty()).ok_or_else(|| {
        EngineError::Config(format!(
            "the VM sandbox needs a guest kernel: build it with `docker build --output type=local,dest=<dir> \
             guest/kernel`, and set {KERNEL_ENV} to <dir>/vmlinux"
        ))
    })?;
    let guest =
        config.tools.get(GUEST).cloned().ok_or_else(|| {
            EngineError::Config(format!("the VM sandbox needs {GUEST}: {}", missing_tool_hint(GUEST)))
        })?;
    Ok(VmConfig {
        vmm: env::var_os(VMM_ENV).map_or_else(|| PathBuf::from(VMM_PROGRAM), PathBuf::from),
        kernel: PathBuf::from(kernel),
        guest,
        images: config.store.join(VM_IMAGES_DIR),
        landlock: vec![config.store.clone()],
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        memory_mib: host_memory_mib().map_or(VM_MEMORY_MIB, |mib| VM_MEMORY_MIB.min(mib / 2)),
        disk_room: VM_DISK_ROOM,
        volume_size: VM_VOLUME_SIZE,
    })
}

fn host_memory_mib() -> Option<u64> {
    let meminfo = std::fs::read_to_string(MEMINFO).ok()?;
    let kib = meminfo.lines().find_map(|line| line.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim();
    kib.parse::<u64>().ok().map(|kib| kib / KIB_PER_MIB)
}

/// How to get a tool that is not found.
/// The target of a static build for this machine's architecture, such as `x86_64-unknown-linux-musl`.
fn musl_target() -> String {
    format!("{}-unknown-linux-musl", env::consts::ARCH)
}

pub(crate) fn missing_tool_hint(name: &str) -> String {
    let target = musl_target();
    match name {
        GHA_RUNNER => format!(
            "build it with `cargo build --release -p {GHA_RUNNER} --target {target}`, or set \
             {GHA_RUNNER_ENV} to the path of a static build"
        ),
        GUEST => format!(
            "build it with `cargo build --release -p {GUEST} --target {target}`, or set {GUEST_ENV} to \
             the path of a static build"
        ),
        _ => format!("this version of Azad does not know where to find {name}"),
    }
}

fn find_static_tool(name: &str, variable: &str) -> Option<PathBuf> {
    if let Some(path) = env::var_os(variable).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let exe = env::current_exe().ok()?;
    let dir = exe.parent()?;
    let musl = dir.parent()?.join(musl_target());
    let candidates = [
        musl.join(dir.file_name()?).join(name),
        musl.join(RELEASE_PROFILE).join(name),
        musl.join(DEBUG_PROFILE).join(name),
        dir.join(name),
    ];
    candidates.into_iter().find(|candidate| is_static_executable(candidate))
}

/// Whether a file is an ELF executable without a program interpreter, so that it runs in any image. A Cargo build puts
/// a dynamic build of the runner next to this program, which fails in an image with another C library.
fn is_static_executable(path: &Path) -> bool {
    let mut head = Vec::new();
    let read = fs::File::open(path).and_then(|file| file.take(ELF_HEAD_BYTES).read_to_end(&mut head));
    read.is_ok() && elf_interpreter_absent(&head)
}

/// The program headers of an executable follow its ELF header, near the start of the file.
const ELF_HEAD_BYTES: u64 = 4096;
const ELF_MAGIC: &[u8] = b"\x7fELF";
const ELF_CLASS_64: u8 = 2;
const ELF_CLASS_OFFSET: usize = 4;
const PROGRAM_HEADERS_OFFSET: usize = 0x20;
const PROGRAM_HEADER_SIZE_OFFSET: usize = 0x36;
const PROGRAM_HEADER_COUNT_OFFSET: usize = 0x38;
const PT_INTERP: u32 = 3;

fn elf_interpreter_absent(bytes: &[u8]) -> bool {
    if !bytes.starts_with(ELF_MAGIC) || bytes.get(ELF_CLASS_OFFSET) != Some(&ELF_CLASS_64) {
        return false;
    }
    let read_u16 = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let read_u32 = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let read_u64 =
        |at: usize| bytes.get(at..at + 8).and_then(|b| usize::try_from(u64::from_le_bytes(b.try_into().ok()?)).ok());
    let (Some(start), Some(size), Some(count)) =
        (read_u64(PROGRAM_HEADERS_OFFSET), read_u16(PROGRAM_HEADER_SIZE_OFFSET), read_u16(PROGRAM_HEADER_COUNT_OFFSET))
    else {
        return false;
    };
    (0..count).all(|index| read_u32(start + index * size).is_some_and(|kind| kind != PT_INTERP))
}

/// The platform of this host, as images name it.
pub fn host_platform() -> String {
    let architecture = match env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{}/{architecture}", env::consts::OS)
}

fn default_root() -> Result<PathBuf, EngineError> {
    let current =
        env::current_dir().map_err(|e| EngineError::Config(format!("cannot read the current directory: {e}")))?;
    let found = current.ancestors().find(|dir| dir.join(ROOT_MARKER).exists()).map(Path::to_path_buf);
    Ok(found.unwrap_or(current))
}

fn default_store_dir() -> Result<PathBuf, EngineError> {
    if let Some(cache) = env::var_os(CACHE_DIR_ENV) {
        return Ok(PathBuf::from(cache).join(STORE_DIR_NAME));
    }
    let home = env::var_os(HOME_ENV).ok_or_else(|| {
        EngineError::Config("neither a store, $AZAD_STORE, $XDG_CACHE_HOME nor $HOME is set".to_owned())
    })?;
    Ok(PathBuf::from(home).join(".cache").join(STORE_DIR_NAME))
}
