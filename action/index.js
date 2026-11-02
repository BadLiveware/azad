// Runs an Azad goal in a GitHub Actions job: prepares the sandbox, passes the cache service's address and token on
// (they are in this process's environment), and sets the outputs from Azad's report. While Azad runs, problem
// matchers turn the plain error lines of tools such as Cargo into annotations.

const { spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const APPARMOR_USERNS = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
const REPORT_FIELDS = ["holds", "needed", "ran", "cached", "failed", "skipped"];
const MATCHER_FILES = [path.join(__dirname, "cargo-matcher.json")];

function input(name) {
  return (process.env[`INPUT_${name.toUpperCase()}`] || "").trim();
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

function main() {
  const workspace = process.env.GITHUB_WORKSPACE || process.cwd();
  const binary = path.resolve(workspace, input("binary"));
  prepareSandbox();
  const reportFile = path.join(process.env.RUNNER_TEMP || os.tmpdir(), `azad-report-${process.pid}.json`);
  const args = ["run", input("goal"), "--isolated", "--cache", input("cache"), "--output", "github", "--report", reportFile, "-j", input("jobs"), "--verbosity", input("verbosity")];
  // AZAD_CONFIG is empty, so that CI never reads personal defaults.
  const env = { ...process.env, AZAD_SIGNING_KEY: input("signing-key"), AZAD_GITHUB_TOKEN: input("github-token"), AZAD_CONFIG: "" };
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

main();
