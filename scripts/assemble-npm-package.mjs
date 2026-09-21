import { execFileSync } from "node:child_process"
import { access, chmod, copyFile, mkdir, readFile, rename, rm } from "node:fs/promises"
import { join } from "node:path"

const targets = [
  ["darwin", "arm64"],
  ["darwin", "x64"],
  ["linux", "arm64"],
  ["linux", "x64"],
  ["win32", "arm64"],
  ["win32", "x64"],
]

const packageManifest = JSON.parse(await readFile("package.json", "utf8"))
const version = packageManifest.version
if (process.env.GITHUB_REF_TYPE === "tag" && process.env.GITHUB_REF_NAME !== `v${version}`) {
  throw new Error(`tag ${process.env.GITHUB_REF_NAME} does not match package version ${version}`)
}

const binaryDirectory = "bin"
const distributionDirectory = "dist"
const npmDistributionDirectory = "npm-dist"
await rm(binaryDirectory, { force: true, recursive: true })
await rm(npmDistributionDirectory, { force: true, recursive: true })
await mkdir(binaryDirectory, { recursive: true })
await mkdir(distributionDirectory, { recursive: true })
await mkdir(npmDistributionDirectory, { recursive: true })

for (const [platform, architecture] of targets) {
  const extension = platform === "win32" ? ".exe" : ""
  const binaryName = `omp-launchpad-${platform}-${architecture}${extension}`
  const source = join(distributionDirectory, `omp-launchpad-${version}-${platform}-${architecture}${extension}`)
  const destination = join(binaryDirectory, binaryName)
  await access(source)
  await copyFile(source, destination)
  if (platform !== "win32") {
    await chmod(destination, 0o755)
  }
}

const npm = process.platform === "win32" ? "npm.cmd" : "npm"
const packOutput = execFileSync(
  npm,
  ["pack", "--ignore-scripts", "--pack-destination", npmDistributionDirectory, "--json"],
  {
    encoding: "utf8",
    shell: process.platform === "win32",
  }
)
const packs = JSON.parse(packOutput)
if (!Array.isArray(packs) || packs.length !== 1 || typeof packs[0].filename !== "string") {
  throw new Error("npm pack returned an unexpected result")
}

const npmArchive = join(npmDistributionDirectory, "omp-launchpad.tgz")
await rename(join(npmDistributionDirectory, packs[0].filename), npmArchive)
const releaseArchive = join(distributionDirectory, `omp-launchpad-${version}.tgz`)
await copyFile(npmArchive, releaseArchive)

console.log(releaseArchive)
console.log(npmArchive)
