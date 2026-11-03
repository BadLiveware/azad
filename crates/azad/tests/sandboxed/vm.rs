//! The microVM sandbox (decisions 0018 and 0019): actions give the same results as in bubblewrap, the runs of a sandbox
//! share one VM without seeing each other's files or processes, and what an action can do wrong ends as a result, not
//! as a hung or broken run.
//!
//! The tests need Cloud Hypervisor (`$AZAD_VMM`, or `cloud-hypervisor` in the PATH), a guest kernel
//! (`$AZAD_VM_KERNEL`), a static build of `azad-guest`, and `/dev/kvm`. Without them each test says so and passes,
//! unless `$AZAD_VM_TESTS` is set, as in CI, where a missing part fails the test.
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::support::{ROOTFS_PATH, TEST_PROGRAMS, host_rootfs};
use azad::engine::GUEST;
use azad::executor::{ExecutorClient, ExecutorService, ServiceSandbox};
use azad::ir::{Action, Digest, Kind, Spec};
use azad::local::{ExecError, Execution, LocalBackend};
use azad::log::{Discard, LogSink, Stream};
use azad::sandbox::{Bubblewrap, Limits, MicroVm, SandboxError, VmConfig};
use azad::scheduler::Backend;
use azad::store::{Selection, Store};
use azad::trust::Trust;
use tempfile::TempDir;

const REQUIRE_ENV: &str = "AZAD_VM_TESTS";
const KERNEL_ENV: &str = "AZAD_VM_KERNEL";
const VMM_ENV: &str = "AZAD_VMM";
const VMM_PROGRAM: &str = "cloud-hypervisor";
const GUEST_ENV: &str = "AZAD_GUEST";
/// Where Cargo puts a static build of the guest init in this workspace.
const GUEST_BUILDS: &[&str] = &[
    "../../target/x86_64-unknown-linux-musl/release/azad-guest",
    "../../target/x86_64-unknown-linux-musl/debug/azad-guest",
];
const KVM_DEVICE: &str = "/dev/kvm";
const EXTRA_PROGRAMS: &[&str] = &["ln", "id", "grep", "sed", "sort", "timeout", "nc"];
const TEST_MEMORY_MIB: u64 = 512;
const TEST_DISK_ROOM: u64 = 1 << 30;
const SMALL_DISK_ROOM: u64 = 64 << 20;
const TEST_VOLUME_SIZE: u64 = 1 << 30;
const TIMEOUT: Duration = Duration::from_secs(60);

/// The parts that the VM sandbox needs, or why a test cannot run.
fn vm_parts() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let kernel = env::var_os(KERNEL_ENV).map(PathBuf::from).ok_or(format!("{KERNEL_ENV} is not set"))?;
    let vmm = env::var_os(VMM_ENV).map(PathBuf::from).or_else(|| find_in_path(VMM_PROGRAM));
    let vmm = vmm.ok_or(format!("{VMM_PROGRAM} is not found; set {VMM_ENV}"))?;
    let guest = env::var_os(GUEST_ENV)
        .map(PathBuf::from)
        .or_else(|| {
            GUEST_BUILDS.iter().map(|path| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)).find(|p| p.is_file())
        })
        .ok_or(format!("no static build of {GUEST} is found; set {GUEST_ENV}"))?;
    OpenOptions::new().read(true).write(true).open(KVM_DEVICE).map_err(|e| format!("{KVM_DEVICE}: {e}"))?;
    Ok((kernel, vmm, guest))
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).map(|dir| dir.join(program)).find(|path| path.is_file())
}

/// The parts, or `None` when a test should not run here.
fn available() -> Option<(PathBuf, PathBuf, PathBuf)> {
    match (vm_parts(), env::var_os(REQUIRE_ENV).is_some()) {
        (Ok(parts), _) => Some(parts),
        (Err(reason), true) => panic!("{REQUIRE_ENV} is set, but the VM sandbox cannot run: {reason}"),
        (Err(reason), false) => {
            eprintln!("skipped: the VM sandbox cannot run here: {reason}");
            None
        }
    }
}

struct Fixture {
    dir: TempDir,
    store: Store,
    root: Digest,
    config: VmConfig,
    bubblewrap: LocalBackend,
    vm: LocalBackend,
}

fn fixture_with(limits: Limits, disk_room: u64) -> Option<Fixture> {
    let (kernel, vmm, guest) = available()?;
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let rootfs = dir.path().join("rootfs");
    let programs: Vec<&str> = TEST_PROGRAMS.iter().chain(EXTRA_PROGRAMS).copied().collect();
    host_rootfs(&rootfs, &programs);
    let root = store.ingest(&rootfs, &Selection::All).unwrap();
    let config = VmConfig {
        vmm,
        kernel,
        guest,
        images: dir.path().join("images"),
        landlock: vec![dir.path().to_path_buf()],
        cpus: 2,
        memory_mib: TEST_MEMORY_MIB,
        disk_room,
        volume_size: TEST_VOLUME_SIZE,
    };
    let vm = LocalBackend::new(store.clone(), Box::new(MicroVm::new(config.clone())), limits.clone()).unwrap();
    let bubblewrap = LocalBackend::new(store.clone(), Box::new(Bubblewrap::default()), limits).unwrap();
    Some(Fixture { dir, store, root, config, bubblewrap, vm })
}

fn fixture() -> Option<Fixture> {
    fixture_with(Limits { timeout: TIMEOUT, ..Limits::default() }, TEST_DISK_ROOM)
}

fn action(f: &Fixture, script: &str, outputs: &[&str]) -> Action {
    Spec {
        kind: Kind::Hermetic,
        platform: "linux/amd64".to_owned(),
        root: f.root,
        command: vec!["bash".to_owned(), "-c".to_owned(), script.to_owned()],
        env: BTreeMap::from([("PATH".to_owned(), ROOTFS_PATH.to_owned())]),
        inputs: BTreeMap::new(),
        outputs: outputs.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>(),
        caches: BTreeMap::new(),
        work: BTreeMap::new(),
        dir: String::new(),
        secrets: Default::default(),
    }
}

fn text(f: &Fixture, digest: &Digest) -> String {
    String::from_utf8(f.store.read(digest).unwrap()).unwrap()
}

fn run(backend: &LocalBackend, action: &Action) -> Execution {
    backend.execute_uncached(action, &Discard).unwrap()
}

/// When the first line of output arrived.
#[derive(Default)]
struct FirstLine(Mutex<Option<Instant>>);

impl LogSink for FirstLine {
    fn line(&self, _stream: Stream, _line: &str) {
        self.0.lock().unwrap().get_or_insert_with(Instant::now);
    }
}

#[test]
fn hermetic_actions_give_the_same_results_in_a_vm_and_in_bubblewrap() {
    let Some(f) = fixture() else { return };
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("input.txt"), "from an input\n").unwrap();
    let files = "mkdir -p out/sub && echo hello > out/greeting && printf '#!/bin/sh\\necho run\\n' > out/sub/tool \
                 && chmod +x out/sub/tool && ln -s greeting out/link && cat /src/input.txt > out/copy";
    let mut actions = vec![
        action(&f, files, &["out"]),
        action(&f, "echo \"$GREETING\" > said && id -u > uid && echo out; echo err >&2; exit 3", &["said", "uid"]),
        action(&f, "(echo > /dev/tcp/127.0.0.1/9) 2>&1 | sed 's/.*: //' | sort -u", &[]),
    ];
    actions[0].inputs.insert("/src".to_owned(), f.store.ingest(&src, &Selection::All).unwrap());
    actions[1].env.insert("GREETING".to_owned(), "hi there".to_owned());

    for a in &actions {
        let (in_bubblewrap, in_vm) = (run(&f.bubblewrap, a), run(&f.vm, a));
        assert_eq!(in_vm.result.exit_code, in_bubblewrap.result.exit_code, "{:?}", a.command);
        assert_eq!(in_vm.result.outputs, in_bubblewrap.result.outputs, "{:?}", a.command);
        assert_eq!(text(&f, &in_vm.result.stdout), text(&f, &in_bubblewrap.result.stdout));
        assert_eq!(text(&f, &in_vm.result.stderr), text(&f, &in_bubblewrap.result.stderr));
    }
    let copy = f.dir.path().join("copy");
    let first = run(&f.vm, &actions[0]);
    f.store.materialize_into(&first.result.outputs["out"], &copy).unwrap();
    assert_eq!(fs::read_to_string(copy.join("copy")).unwrap(), "from an input\n");
    assert_eq!(fs::read_link(copy.join("link")).unwrap(), PathBuf::from("greeting"));

    let first_line = FirstLine::default();
    let started = Instant::now();
    f.vm.execute_uncached(&action(&f, "echo ready", &[]), &first_line).unwrap();
    let boot = first_line.0.lock().unwrap().map(|at| at - started);
    eprintln!("the first line of a command in a VM arrived after {boot:?}");
    assert!(boot.is_some());
}

/// The ID that the guest's kernel picked at boot.
const BOOT_ID: &str = "cat /proc/sys/kernel/random/boot_id";

#[test]
fn runs_share_one_vm_at_the_same_time_and_each_sees_only_its_own_files() {
    let Some(f) = fixture() else { return };
    let runs: Vec<Execution> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let script = format!("mkdir mine && echo {i} > mine/{i} && sleep 0.3 && ls && ls mine && {BOOT_ID}");
                let a = action(&f, &script, &["mine"]);
                let vm = &f.vm;
                scope.spawn(move || run(vm, &a))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    });
    let boots: BTreeSet<String> =
        runs.iter().map(|r| text(&f, &r.result.stdout).lines().last().unwrap().to_owned()).collect();
    assert_eq!(boots.len(), 1, "the runs share one VM: {boots:?}");
    for (i, r) in runs.iter().enumerate() {
        let said: Vec<String> = text(&f, &r.result.stdout).lines().take(2).map(str::to_owned).collect();
        assert_eq!(said, ["mine".to_owned(), i.to_string()], "a run sees only its own work directory");
    }
}

/// The plan (contract 2): a client opens a sandbox group in an executor service and runs actions in it; one VM serves
/// the group.
#[test]
fn actions_run_through_the_executor_service_in_one_vm_per_group() {
    let Some(f) = fixture() else { return };
    let socket = f.dir.path().join("executor.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let limits = Limits { timeout: TIMEOUT, ..Limits::default() };
    let service = ExecutorService::new(f.store.clone(), ServiceSandbox::Vm(f.config.clone()), limits);
    std::thread::spawn(move || service.serve(&listener));
    let client = ExecutorClient::new(socket, f.store.clone(), Trust::Main);
    let outputs: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..3)
            .map(|i| {
                let a = action(&f, &format!("echo run {i}; {BOOT_ID}"), &[]);
                let client = &client;
                scope.spawn(move || client.execute(&a, &Discard).unwrap())
            })
            .collect();
        handles.into_iter().map(|handle| text(&f, &handle.join().unwrap().result.stdout)).collect()
    });
    let boots: BTreeSet<&str> = outputs.iter().map(|out| out.lines().last().unwrap()).collect();
    assert_eq!(boots.len(), 1, "one VM served the group: {outputs:?}");
    for (i, out) in outputs.iter().enumerate() {
        assert_eq!(out.lines().next(), Some(format!("run {i}").as_str()));
    }
    let again = action(&f, "echo run 0; cat /proc/sys/kernel/random/boot_id", &[]);
    assert!(client.cached(&again).unwrap().is_some(), "the client keeps the results in the action cache");
}

#[test]
fn processes_that_a_command_leaves_end_with_its_run() {
    let Some(f) = fixture() else { return };
    let started = Instant::now();
    run(&f.vm, &action(&f, "sleep 1000 & echo left a process", &[]));
    assert!(started.elapsed() < Duration::from_secs(20), "the run did not wait for the process that it left");
    let script = "for comm in /proc/[0-9]*/comm; do cat $comm; done 2>/dev/null | grep -c '^sleep$' || true";
    assert_eq!(text(&f, &run(&f.vm, &action(&f, script, &[])).result.stdout), "0\n");
}

/// A cache volume is a disk in a VM: what one run writes, the next run reads, as with the volume's directory in
/// bubblewrap.
#[test]
fn a_cache_volume_keeps_its_files_between_runs_in_a_vm() {
    let Some(f) = fixture() else { return };
    let mut a = action(
        &f,
        "cat /cache/count 2>/dev/null || echo 0; echo $(( $(cat /cache/count 2>/dev/null || echo 0) + 1 )) > /cache/count; id -u",
        &[],
    );
    a.caches.insert("/cache".to_owned(), "counter".to_owned());
    let mut b = a.clone();
    b.caches = BTreeMap::from([("/work/target".to_owned(), "counter-in-work".to_owned())]);
    b.command[2] = "echo kept > target/file; ls target".to_owned();
    let said: Vec<String> = (0..3).map(|_| text(&f, &run(&f.vm, &a).result.stdout)).collect();
    let uid = said[0].lines().nth(1).unwrap().to_owned();
    assert_eq!(said, ["0", "1", "2"].map(|count| format!("{count}\n{uid}\n")));
    assert_eq!(text(&f, &run(&f.vm, &b).result.stdout), "file\n", "a volume can be inside /work");
}

#[test]
fn writes_outside_work_and_tmp_fail_as_in_bubblewrap() {
    let Some(f) = fixture() else { return };
    let script = "for path in /usr/bin/new /new /tmp/new /work/new; do touch $path 2>/dev/null && echo $path; done";
    let a = action(&f, script, &[]);
    let (in_bubblewrap, in_vm) = (run(&f.bubblewrap, &a), run(&f.vm, &a));
    assert_eq!(text(&f, &in_vm.result.stdout), "/tmp/new\n/work/new\n");
    assert_eq!(text(&f, &in_vm.result.stdout), text(&f, &in_bubblewrap.result.stdout));
}

#[test]
fn a_hermetic_action_in_a_vm_has_no_network() {
    let Some(f) = fixture() else { return };
    let a = action(&f, "(echo > /dev/tcp/1.1.1.1/53) 2>/dev/null && echo connected || echo no network", &[]);
    assert_eq!(text(&f, &run(&f.vm, &a).result.stdout), "no network\n");
}

#[test]
fn a_command_that_is_not_found_is_reported() {
    let Some(f) = fixture() else { return };
    let mut a = action(&f, "", &[]);
    a.command = vec!["no-such-program".to_owned()];
    let error = f.vm.execute_uncached(&a, &Discard).unwrap_err();
    assert!(
        matches!(&error, ExecError::Sandbox(SandboxError::CommandNotFound { command, .. }) if command == "no-such-program"),
        "{error}"
    );
}

#[test]
fn an_action_that_runs_too_long_is_stopped() {
    let limit = Duration::from_secs(3);
    let Some(f) = fixture_with(Limits { timeout: limit, ..Limits::default() }, TEST_DISK_ROOM) else { return };
    let started = Instant::now();
    let error = f.vm.execute_uncached(&action(&f, "sleep 60", &[]), &Discard).unwrap_err();
    assert!(matches!(error, ExecError::TimedOut(_)), "{error}");
    assert!(started.elapsed() < limit * 3, "the VM was stopped soon after the limit");
}

#[test]
fn a_command_that_uses_more_memory_than_the_vm_has_fails_and_the_run_reports_it() {
    let Some(f) = fixture() else { return };
    let run = run(&f.vm, &action(&f, "s=x; while true; do s=$s$s; done", &[]));
    assert_ne!(run.result.exit_code, 0);
}

#[test]
fn a_full_run_disk_fails_the_command_and_the_run_reports_it() {
    let limits = Limits { timeout: TIMEOUT, ..Limits::default() };
    let Some(f) = fixture_with(limits, SMALL_DISK_ROOM) else { return };
    let a = action(&f, "head -c 200000000 /dev/zero > big", &[]);
    let run = run(&f.vm, &a);
    assert_ne!(run.result.exit_code, 0);
    assert!(text(&f, &run.result.stderr).contains("No space left"), "{}", text(&f, &run.result.stderr));
}

/// A kernel without a working init panics; the host stops the VM and reports it, instead of waiting for a guest that
/// Cloud Hypervisor boots again and again.
#[test]
fn a_guest_that_panics_is_stopped_and_reported() {
    let Some((kernel, vmm, _)) = available() else { return };
    let dir = TempDir::new().unwrap();
    let not_a_program = dir.path().join("not-a-program");
    fs::write(&not_a_program, "not a program").unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let rootfs = dir.path().join("rootfs");
    host_rootfs(&rootfs, TEST_PROGRAMS);
    let root = store.ingest(&rootfs, &Selection::All).unwrap();
    let config = VmConfig {
        vmm,
        kernel,
        guest: not_a_program,
        images: dir.path().join("images"),
        landlock: vec![dir.path().to_path_buf()],
        cpus: 1,
        memory_mib: TEST_MEMORY_MIB,
        disk_room: TEST_DISK_ROOM,
        volume_size: TEST_VOLUME_SIZE,
    };
    let limits = Limits { timeout: TIMEOUT, ..Limits::default() };
    let vm = LocalBackend::new(store.clone(), Box::new(MicroVm::new(config)), limits).unwrap();
    let a = Spec {
        kind: Kind::Hermetic,
        platform: "linux/amd64".to_owned(),
        root,
        command: vec!["true".to_owned()],
        env: BTreeMap::from([("PATH".to_owned(), ROOTFS_PATH.to_owned())]),
        inputs: BTreeMap::new(),
        outputs: BTreeSet::new(),
        caches: BTreeMap::new(),
        work: BTreeMap::new(),
        dir: String::new(),
        secrets: Default::default(),
    };
    let started = Instant::now();
    let error = vm.execute_uncached(&a, &Discard).unwrap_err();
    assert!(error.to_string().contains("the guest panicked"), "{error}");
    assert!(started.elapsed() < TIMEOUT / 2, "the VM was stopped at the panic, not at the time limit");
}

/// An action with network, whose `script` gets the targets to probe in `$TARGETS` and the port in `$PORT`.
fn networked(f: &Fixture, script: &str) -> Action {
    let mut a = action(f, script, &[]);
    a.kind = Kind::Impure;
    a
}

/// Tries each target at once, and says for each whether a connection to it was made.
const PROBE: &str = "for target in $TARGETS; do \
    (timeout 3 bash -c \"echo > /dev/tcp/$target/$PORT\" 2>/dev/null && echo reached $target || echo blocked $target) & \
    done; wait";
const SLIRP_HOST_LOOPBACK: &str = "10.0.2.2";
const NAMESPACE_ADDRESS: &str = "172.31.0.1";
const METADATA_SERVER: &str = "169.254.169.254";

/// The addresses of this host's interfaces, IPv4 and global IPv6, as a guest could name them.
fn host_addresses() -> Vec<String> {
    let trie = fs::read_to_string("/proc/net/fib_trie").unwrap();
    let lines: Vec<&str> = trie.lines().collect();
    let mut found: BTreeSet<String> = lines
        .windows(2)
        .filter(|pair| pair[1].trim() == "/32 host LOCAL")
        .filter_map(|pair| pair[0].trim().strip_prefix("|-- ").map(str::to_owned))
        .collect();
    let inet6 = fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
    for hex in inet6.lines().filter_map(|line| line.split_whitespace().next()).filter(|hex| !hex.starts_with("fe80")) {
        found.insert(std::net::Ipv6Addr::from(u128::from_str_radix(hex, 16).unwrap()).to_string());
    }
    found.into_iter().collect()
}

/// The guest reaches nothing of the host: not its addresses, not its loopback through slirp4netns, not the namespace
/// that holds the VM's tap device, and not a metadata server. The same probe in bubblewrap, which shares the host's
/// network, shows that the probe can tell.
#[test]
fn an_action_with_network_cannot_reach_the_host() {
    let Some(f) = fixture() else { return };
    let listener = TcpListener::bind("[::]:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let host = host_addresses();
    let mut targets = host.clone();
    targets.extend([SLIRP_HOST_LOOPBACK, NAMESPACE_ADDRESS, METADATA_SERVER].map(str::to_owned));
    let mut a = networked(&f, PROBE);
    a.env.insert("TARGETS".to_owned(), targets.join(" "));
    a.env.insert("PORT".to_owned(), port);

    let in_vm: BTreeSet<String> = text(&f, &run(&f.vm, &a).result.stdout).lines().map(str::to_owned).collect();
    let expected: BTreeSet<String> = targets.iter().map(|target| format!("blocked {target}")).collect();
    assert_eq!(in_vm, expected);
    assert!(matches!(listener.accept(), Err(e) if e.kind() == ErrorKind::WouldBlock), "the listener got nothing");

    let in_bubblewrap = text(&f, &run(&f.bubblewrap, &a).result.stdout);
    for address in &host {
        assert!(in_bubblewrap.contains(&format!("reached {address}")), "{in_bubblewrap}");
    }
}

/// Each VM with network has its own namespaces, so a guest cannot reach another VM: not at the address that the other
/// guest has, which is its own, and not through the host.
#[test]
fn an_action_with_network_cannot_reach_another_vm() {
    let Some(f) = fixture() else { return };
    let Some(other) = fixture() else { return };
    let listen = networked(&other, "timeout 8 nc -l 7000 && echo got a connection || echo nothing came");
    let mut probe = networked(&f, PROBE);
    probe.env.insert("TARGETS".to_owned(), ["172.31.0.2", NAMESPACE_ADDRESS, SLIRP_HOST_LOOPBACK].join(" "));
    probe.env.insert("PORT".to_owned(), "7000".to_owned());
    let (listened, probed) = std::thread::scope(|scope| {
        let listening = scope.spawn(|| run(&other.vm, &listen));
        std::thread::sleep(Duration::from_secs(2));
        let probed = run(&f.vm, &probe);
        (listening.join().unwrap(), probed)
    });
    assert!(!text(&f, &probed.result.stdout).contains("reached"), "{}", text(&f, &probed.result.stdout));
    assert_eq!(text(&other, &listened.result.stdout), "nothing came\n");
}

/// The guest reaches the internet, and names resolve. It needs the internet, so it passes without it.
#[test]
fn an_action_with_network_reaches_the_internet() {
    let Some(f) = fixture() else { return };
    let address = ("github.com", 443).to_socket_addrs().ok().and_then(|mut addresses| addresses.next());
    let reachable = address.is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_secs(5)).is_ok());
    if !reachable {
        eprintln!("skipped: this host does not reach the internet");
        return;
    }
    let a = networked(&f, "timeout 10 bash -c 'echo > /dev/tcp/github.com/443' && echo reached");
    assert_eq!(text(&f, &run(&f.vm, &a).result.stdout), "reached\n");
}
