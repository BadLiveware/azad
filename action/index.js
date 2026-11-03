// Runs an Azad goal in a GitHub Actions job: installs a release of Azad, prepares the sandbox, passes the cache
// service's address and token on (they are in this process's environment), and sets the outputs from Azad's report.
// While Azad runs, problem matchers turn the plain error lines of tools such as Cargo into annotations.

const { spawn, spawnSync } = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const APPARMOR_USERNS = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
const REPORT_FIELDS = ["holds", "needed", "ran", "cached", "failed", "skipped"];
const MATCHER_FILES = [path.join(__dirname, "cargo-matcher.json")];
const GITHUB_URL = "https://github.com";
const AZAD_REPOSITORY = "BadLiveware/azad";
const LATEST = "latest";
const RELEASE_TAG = /^v\d+\.\d{8}\.\d+$/;
// The GHA runner goes next to azad, which finds it there when a goal is a workflow file.
const PROGRAMS = ["azad", "azad-gha-runner"];
const ARCHITECTURES = { x64: "x86_64", arm64: "aarch64" };
const INSTALLED_MARK = ".installed";

function input(name) {
  return (process.env[`INPUT_${name.toUpperCase()}`] || "").trim();
}

function releaseTarget() {
  const architecture = ARCHITECTURES[process.arch];
  if (process.platform !== "linux" || !architecture) {
    throw new Error(`the Azad action runs on Linux runners on x64 or arm64, not ${process.platform} on ${process.arch}`);
  }
  return `${architecture}-unknown-linux-musl`;
}

// The version input; else the release that the workflow pins the action to, so that the action and azad match; else
// the latest release.
function wantedVersion() {
  const version = input("version");
  if (version) {
    return version.replace(/^v/, "");
  }
  const ref = process.env.GITHUB_ACTION_REF || "";
  if (RELEASE_TAG.test(ref)) {
    return ref.slice(1);
  }
  return LATEST;
}

// GitHub answers a request for the latest release with a redirect to the page of its tag, which needs no token.
async function latestVersion(repository) {
  const url = `${GITHUB_URL}/${repository}/releases/${LATEST}`;
  const response = await fetch(url, { redirect: "manual" });
  const tag = (response.headers.get("location") || "").split("/").pop();
  if (!RELEASE_TAG.test(tag)) {
    throw new Error(`${url} names no release: ${response.status} ${response.statusText}`);
  }
  return tag.slice(1);
}

async function download(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url}: ${response.status} ${response.statusText}`);
  }
  return Buffer.from(await response.arrayBuffer());
}

async function installProgram(release, program, target, dir) {
  const archive = `${program}-${target}.tar.gz`;
  const [bytes, checksum] = await Promise.all([download(`${release}/${archive}`), download(`${release}/${archive}.sha256`)]);
  const expected = checksum.toString("utf8").split(/\s+/)[0];
  const actual = crypto.createHash("sha256").update(bytes).digest("hex");
  if (actual !== expected) {
    throw new Error(`${release}/${archive} has the SHA-256 ${actual}, but its checksum file says ${expected}`);
  }
  const file = path.join(dir, archive);
  fs.writeFileSync(file, bytes);
  sh(`tar -xzf '${file}' -C '${dir}'`);
  fs.rmSync(file);
}

// Installs the release's programs in the runner's tool cache, which a self-hosted runner keeps between jobs, and
// returns the path of azad.
async function installRelease() {
  const target = releaseTarget();
  const repository = process.env.GITHUB_ACTION_REPOSITORY || AZAD_REPOSITORY;
  let version = wantedVersion();
  if (version === LATEST) {
    version = await latestVersion(repository);
  }
  const dir = path.join(process.env.RUNNER_TOOL_CACHE || os.tmpdir(), "azad", version, target);
  const release = `${GITHUB_URL}/${repository}/releases/download/v${version}`;
  console.log(`Azad ${version} from ${release}`);
  if (!fs.existsSync(path.join(dir, INSTALLED_MARK))) {
    fs.rmSync(dir, { recursive: true, force: true });
    fs.mkdirSync(dir, { recursive: true });
    await Promise.all(PROGRAMS.map((program) => installProgram(release, program, target, dir)));
    fs.writeFileSync(path.join(dir, INSTALLED_MARK), "");
  }
  return path.join(dir, "azad");
}

// A binary that the workflow built, as Azad's own CI does, or a release.
async function azad(workspace) {
  if (!input("binary")) {
    return installRelease();
  }
  if (input("version")) {
    throw new Error("give the action a version or a binary, not both");
  }
  return path.resolve(workspace, input("binary"));
}

function sh(command) {
  const result = spawnSync("sh", ["-c", command], { stdio: "inherit" });
  if (result.status !== 0) {
    throw new Error(`${command}: exit ${result.status}`);
  }
}

// The sandbox is bubblewrap in an unprivileged user namespace, which Ubuntu 24.04 blocks with AppArmor.
function prepareSandbox() {
  if (spawnSync("sh", ["-c", "command -v bwrap"]).status !== 0) {
    sh("sudo apt-get install -y --no-install-recommends bubblewrap");
  }
  if (fs.existsSync(APPARMOR_USERNS) && fs.readFileSync(APPARMOR_USERNS, "utf8").trim() !== "0") {
    sh(`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`);
  }
}

// The runner reads standard output and standard error separately, so a command on standard output can act before
// lines that Azad wrote earlier. Azad writes to standard error, so the matcher commands go there, in order with them.
function addMatchers() {
  for (const file of MATCHER_FILES) {
    console.error(`::add-matcher::${file}`);
  }
}

function removeMatchers() {
  for (const file of MATCHER_FILES) {
    for (const matcher of JSON.parse(fs.readFileSync(file, "utf8")).problemMatcher) {
      console.error(`::remove-matcher owner=${matcher.owner}::`);
    }
  }
}

function setOutputs(reportFile) {
  if (!fs.existsSync(reportFile)) {
    return;
  }
  const report = JSON.parse(fs.readFileSync(reportFile, "utf8"));
  const lines = REPORT_FIELDS.map((field) => `${field}=${report[field]}\n`).join("");
  fs.appendFileSync(process.env.GITHUB_OUTPUT, lines);
}

async function main() {
  const workspace = process.env.GITHUB_WORKSPACE || process.cwd();
  const binary = await azad(workspace);
  prepareSandbox();
  const reportFile = path.join(process.env.RUNNER_TEMP || os.tmpdir(), `azad-report-${process.pid}.json`);
  const args = ["run", input("goal"), "--isolated", "--cache", input("cache"), "--output", "github", "--report", reportFile, "-j", input("jobs"), "--verbosity", input("verbosity")];
  // AZAD_CONFIG is empty, so that CI never reads personal defaults.
  const env = { ...process.env, AZAD_SIGNING_KEY: input("signing-key"), AZAD_GITHUB_TOKEN: input("github-token"), AZAD_CONFIG: "" };
  if (input("force") === "true") {
    args.push("--force");
  }
  for (const filter of ["include", "exclude"]) {
    if (input(filter)) {
      args.push(`--${filter}`, input(filter));
    }
  }
  // A checkout is shallow, so GitHub's API gives the history that it lacks.
  if (input("github-token") && process.env.GITHUB_REPOSITORY) {
    args.push("--github-history", process.env.GITHUB_REPOSITORY);
    args.push("--github-graphql-url", process.env.GITHUB_GRAPHQL_URL || "https://api.github.com/graphql");
  }
  addMatchers();
  const child = spawn(binary, args, { stdio: "inherit", env, cwd: workspace });
  // On a cancel, Azad prints the output that it kept, so pass the signal on.
  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.on(signal, () => child.kill(signal));
  }
  child.on("error", (error) => {
    removeMatchers();
    console.log(`::error title=Azad::cannot start ${binary}: ${error.message}`);
    process.exit(1);
  });
  child.on("exit", (code, signal) => {
    removeMatchers();
    setOutputs(reportFile);
    if (signal) {
      process.exit(1);
    }
    process.exit(code);
  });
}

main().catch((error) => {
  console.log(`::error title=Azad::${error.message}`);
  process.exit(1);
});
