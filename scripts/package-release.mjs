import { execFileSync, spawnSync } from "node:child_process"
import { access, chmod, copyFile, mkdir, readFile, rename, rm } from "node:fs/promises"
import { join } from "node:path"

const [target, platform, architecture] = process.argv.slice(2)
const platforms = new Set(["darwin", "linux", "win32"])
const architectures = new Set(["arm64", "x64"])

if (!target || !/^[a-zA-Z0-9_-]+$/.test(target)) {
  throw new Error("target must be a Rust target triple")
}
if (!platforms.has(platform)) {
  throw new Error(`unsupported platform: ${platform}`)
}
if (!architectures.has(architecture)) {
  throw new Error(`unsupported architecture: ${architecture}`)
}

const packageManifest = JSON.parse(await readFile("package.json", "utf8"))
const version = packageManifest.version
if (process.env.GITHUB_REF_TYPE === "tag" && process.env.GITHUB_REF_NAME !== `v${version}`) {
  throw new Error(`tag ${process.env.GITHUB_REF_NAME} does not match package version ${version}`)
}

const extension = platform === "win32" ? ".exe" : ""
const source = join("target", target, "release", `omp-launchpad${extension}`)
await access(source)
const smoke = spawnSync(source, [], {
  encoding: "utf8",
  input: '{\"op\":\"resource_view\"}',
})
if (smoke.error) {
  throw smoke.error
}
if (smoke.status !== 0) {
  throw new Error(`native bridge exited with status ${smoke.status}: ${smoke.stderr.trim()}`)
}
const smokeResponse = JSON.parse(smoke.stdout)
if (smokeResponse.ok !== false || typeof smokeResponse.error !== "string") {
  throw new Error("native bridge did not return the expected error protocol")
}

const binaryName = `omp-launchpad-${platform}-${architecture}${extension}`
const binaryDirectory = "bin"
const distributionDirectory = "dist"
await rm(binaryDirectory, { force: true, recursive: true })
await rm(distributionDirectory, { force: true, recursive: true })
await mkdir(binaryDirectory, { recursive: true })
await mkdir(distributionDirectory, { recursive: true })

const packagedBinary = join(binaryDirectory, binaryName)
await copyFile(source, packagedBinary)
if (platform !== "win32") {
  await chmod(packagedBinary, 0o755)
}

const releaseStem = `omp-launchpad-${version}-${platform}-${architecture}`
const releaseBinary = join(distributionDirectory, `${releaseStem}${extension}`)
await copyFile(packagedBinary, releaseBinary)
if (platform !== "win32") {
  await chmod(releaseBinary, 0o755)
}

const npm = process.platform === "win32" ? "npm.cmd" : "npm"
const packOutput = execFileSync(
  npm,
  ["pack", "--ignore-scripts", "--pack-destination", distributionDirectory, "--json"],
  { encoding: "utf8" }
)
const packs = JSON.parse(packOutput)
if (!Array.isArray(packs) || packs.length !== 1 || typeof packs[0].filename !== "string") {
  throw new Error("npm pack returned an unexpected result")
}
const releasePackage = join(distributionDirectory, `${releaseStem}.tgz`)
await rename(join(distributionDirectory, packs[0].filename), releasePackage)

console.log(releaseBinary)
console.log(releasePackage)
