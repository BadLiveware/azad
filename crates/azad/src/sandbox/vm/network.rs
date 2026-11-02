//! The network of a VM, without root: the VMM runs in a user namespace and a network namespace of its own, where it has
//! the right to make the VM's tap device. nftables rules in that namespace let the guest out to the internet only:
//! never to the host, to other VMs, to metadata addresses or to private networks. slirp4netns connects the namespace to
//! the host's network, as rootless containers do.
//!
//! Each VM with network has its own namespaces, so VMs cannot reach each other. The one exception is DNS: the guest
//! asks slirp4netns's resolver, which asks the host's.

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::sandbox::SandboxError;
use crate::sandbox::vm::io_error;
use crate::sandbox::vm::namespaces::{in_namespaces, in_new_namespaces, user_and_group};

const HOLDER: &str = "sleep";
const HOLDER_ARGUMENT: &str = "infinity";
const NFT: &str = "nft";
const SYSCTL: &str = "sysctl";
const SLIRP: &str = "slirp4netns";
const SLIRP_LOG: &str = "slirp4netns.log";
/// The VM's tap device, in the namespace.
const TAP: &str = "azad0";
/// slirp4netns's device, in the namespace.
const UPLINK: &str = "tap0";
const UPLINK_MTU: &str = "--mtu=65520";
const NAMESPACE_ADDRESS: &str = "172.31.0.1";
const GUEST_ADDRESS: &str = "172.31.0.2";
const NETMASK: &str = "255.255.255.252";
const GUEST_DEVICE: &str = "eth0";
/// slirp4netns's DNS resolver.
const RESOLVER: &str = "10.0.2.3";
const DNS_PORT: u16 = 53;
/// Addresses that a guest never reaches: this network, private networks (which hold slirp4netns's addresses and the
/// host's loopback behind them), shared address space (which holds a tailnet), loopback, link-local (which holds
/// metadata servers), multicast and reserved.
const FORBIDDEN: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "224.0.0.0/3",
];
const FIB_TRIE: &str = "/proc/net/fib_trie";
const LOCAL_ADDRESS: &str = "/32 host LOCAL";
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// In `/proc/<pid>/net/route`, a default route through the uplink.
const DEFAULT_ROUTE: &str = "00000000";

pub(super) struct GuestNetwork {
    holder: Child,
    slirp: Child,
    user: File,
    net: File,
}

impl GuestNetwork {
    /// Makes the namespaces with their rules, and connects them to the host's network. `dir` keeps slirp4netns's log.
    pub(super) fn start(dir: &Path) -> Result<Self, SandboxError> {
        let holder = in_new_namespaces(&mut Command::new(HOLDER))
            .arg(HOLDER_ARGUMENT)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| tool_error(HOLDER, e))?;
        let pid = holder.id();
        let proc = Path::new("/proc").join(pid.to_string());
        map_user(&proc)?;
        let open = |path: &Path| File::open(path).map_err(|e| io_error(path, e));
        let (user, net) = (open(&proc.join("ns/user"))?, open(&proc.join("ns/net"))?);
        let log_path = dir.join(SLIRP_LOG);
        let log = File::create(&log_path).map_err(|e| io_error(&log_path, e))?;
        let mut network = GuestNetwork { holder, slirp: placeholder()?, user, net };
        network.run_inside(Command::new(SYSCTL).args(["-q", "-w", "net.ipv4.ip_forward=1"]), None)?;
        network.run_inside(Command::new(NFT).args(["-f", "-"]), Some(&ruleset(&host_addresses())))?;
        network.slirp = Command::new(SLIRP)
            .args(["--configure", UPLINK_MTU, "--disable-host-loopback", &pid.to_string(), UPLINK])
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| io_error(&log_path, e))?)
            .stderr(log)
            .spawn()
            .map_err(|e| tool_error(SLIRP, e))?;
        wait_for_uplink(&proc.join("net/route"), &log_path)?;
        Ok(network)
    }

    /// A command that runs in the namespaces, such as the VMM.
    pub(super) fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        in_namespaces(&mut command, self.user.as_raw_fd(), self.net.as_raw_fd());
        command
    }

    /// The kernel's own configuration of the guest's address and route.
    pub(super) fn kernel_argument(&self) -> String {
        format!("ip={GUEST_ADDRESS}::{NAMESPACE_ADDRESS}:{NETMASK}::{GUEST_DEVICE}:off")
    }

    /// The VM's network device, for Cloud Hypervisor, which makes the tap device in the namespace.
    pub(super) fn device_argument(&self) -> String {
        format!("tap={TAP},ip={NAMESPACE_ADDRESS},mask={NETMASK}")
    }

    pub(super) fn resolv_conf() -> String {
        format!("nameserver {RESOLVER}\n")
    }

    fn run_inside(&self, command: &mut Command, input: Option<&str>) -> Result<(), SandboxError> {
        let program = command.get_program().to_string_lossy().into_owned();
        in_namespaces(command, self.user.as_raw_fd(), self.net.as_raw_fd());
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| tool_error(&program, e))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(input.as_bytes()).map_err(SandboxError::Spawn)?;
        }
        let output = child.wait_with_output().map_err(SandboxError::Spawn)?;
        match output.status.success() {
            true => Ok(()),
            false => Err(SandboxError::Setup(format!(
                "{program} failed in the VM's network namespace: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }
}

impl Drop for GuestNetwork {
    fn drop(&mut self) {
        for child in [&mut self.slirp, &mut self.holder] {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

/// A child that has ended, in the place of slirp4netns until it starts.
fn placeholder() -> Result<Child, SandboxError> {
    let mut child = Command::new("true").spawn().map_err(SandboxError::Spawn)?;
    child.wait().map_err(SandboxError::Spawn)?;
    Ok(child)
}

/// Maps this user to root in the new user namespace, which gives the namespace's processes their capabilities
/// there, and nothing on the host.
fn map_user(proc: &Path) -> Result<(), SandboxError> {
    let (uid, gid) = user_and_group();
    for (file, content) in
        [("setgroups", "deny".to_owned()), ("uid_map", format!("0 {uid} 1")), ("gid_map", format!("0 {gid} 1"))]
    {
        let path = proc.join(file);
        fs::write(&path, content).map_err(|e| io_error(&path, e))?;
    }
    Ok(())
}

/// The rules of a guest, in the VM's own namespace: the guest reaches the internet and the DNS resolver, and nothing
/// else.
fn ruleset(host_addresses: &[String]) -> String {
    let forbidden = FORBIDDEN.join(", ");
    let host_rule = match host_addresses.is_empty() {
        true => String::new(),
        false => format!("iifname \"{TAP}\" ip daddr {{ {} }} drop", host_addresses.join(", ")),
    };
    format!(
        r#"table inet azad {{
  chain forward {{
    type filter hook forward priority filter; policy drop;
    iifname "{UPLINK}" oifname "{TAP}" ct state established,related accept
    iifname "{TAP}" meta nfproto ipv6 drop
    iifname "{TAP}" ip daddr {RESOLVER} meta l4proto {{ tcp, udp }} th dport {DNS_PORT} accept
    iifname "{TAP}" ip daddr {{ {forbidden} }} drop
    {host_rule}
    iifname "{TAP}" oifname "{UPLINK}" accept
  }}
  chain input {{
    type filter hook input priority filter; policy accept;
    iifname "{TAP}" drop
  }}
  chain postrouting {{
    type nat hook postrouting priority srcnat; policy accept;
    oifname "{UPLINK}" masquerade
  }}
}}
"#
    )
}

/// The IPv4 addresses of the host's interfaces, which slirp4netns would reach as the host.
fn host_addresses() -> Vec<String> {
    let Ok(trie) = fs::read_to_string(FIB_TRIE) else { return Vec::new() };
    let lines: Vec<&str> = trie.lines().collect();
    let mut found: Vec<String> = lines
        .windows(2)
        .filter(|pair| pair[1].trim() == LOCAL_ADDRESS)
        .filter_map(|pair| pair[0].trim().strip_prefix("|-- ").map(str::to_owned))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// slirp4netns makes the uplink and its default route when it is ready.
fn wait_for_uplink(routes: &Path, log: &Path) -> Result<(), SandboxError> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let ready = || {
        fs::read_to_string(routes).is_ok_and(|text| {
            text.lines().any(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                fields.first() == Some(&UPLINK) && fields.get(1) == Some(&DEFAULT_ROUTE)
            })
        })
    };
    while !ready() {
        if Instant::now() >= deadline {
            let said = fs::read_to_string(log).unwrap_or_default();
            return Err(SandboxError::Setup(format!("{SLIRP} did not connect the VM's network: {}", said.trim())));
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

fn tool_error(program: &str, error: std::io::Error) -> SandboxError {
    match error.kind() {
        std::io::ErrorKind::NotFound => {
            SandboxError::Setup(format!("{program} is not installed; the VM sandbox needs it for actions with network"))
        }
        _ => SandboxError::Spawn(error),
    }
}
