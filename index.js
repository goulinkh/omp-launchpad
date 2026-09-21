import { existsSync, rmSync } from "node:fs"
import { homedir } from "node:os"
import { basename, dirname, isAbsolute, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const PLUGIN_DIR = dirname(fileURLToPath(import.meta.url))
const BRIDGE_PATH = join(PLUGIN_DIR, "bridge.py")
const RESULT_MARKER = "__OMP_LAUNCHPAD__"
const MAX_FILE_BYTES = 2 * 1024 * 1024

export default function launchpadExtension(pi) {
  const z = pi.zod

  pi.setLabel("Launchpad")

  pi.registerTool({
    name: "read",
    label: "Read",
    description:
      "Read files, directories, archives, databases, web URLs, OMP internal resources, and Launchpad resources. " +
      "Launchpad forms: lp://bugs/<id>, lp://<project>, lp://~owner/project/+git/repo, and " +
      "lp://~owner/project/+git/repo/+merge/<id>[/diff]. Add ?comments=0 to omit comments.",
    parameters: z.object({
      path: z.string(),
      i: z.string().optional(),
    }),
    loadMode: "essential",
    approval: "read",
    async execute(_toolCallId, params, signal, onUpdate, ctx) {
      if (!isLaunchpadUrl(params.path)) {
        if (!ctx.invokeTool) {
          throw new Error("The native read tool is unavailable for delegation")
        }
        return ctx.invokeTool(params, { signal, onUpdate })
      }
      return bridgeResult(await runBridge({ op: "resource_view", target: params.path }, signal, ctx.cwd))
    },
  })

  const sharedParameters = {
    target: z.string().optional(),
    repository: z.string().optional(),
    target_repository: z.string().optional(),
    path: z.string().optional(),
    branch: z.string().optional(),
    query: z.string().optional(),
    status: z.union([z.string(), z.array(z.string())]).optional(),
    importance: z.union([z.string(), z.array(z.string())]).optional(),
    tags: z.array(z.string()).optional(),
    limit: z.number().optional(),
    title: z.string().optional(),
    description: z.string().optional(),
    information_type: z.string().optional(),
    source_ref: z.string().optional(),
    target_ref: z.string().optional(),
    commit_message: z.string().optional(),
    needs_review: z.boolean().optional(),
    body: z.string().optional(),
    subject: z.string().optional(),
    vote: z
      .enum(["Approve", "Needs Fixing", "Needs Information", "Abstain", "Disapprove", "Needs Resubmitting"])
      .optional(),
    directory: z.string().optional(),
    force_with_lease: z.boolean().optional(),
    i: z.string().optional(),
  }

  pi.registerTool({
    name: "launchpad",
    label: "Launchpad",
    description:
      "Read Launchpad through lp-shell/launchpadlib. Supports repository and resource views, repository file " +
      "reads, and bug or merge-proposal search. Prefer read with lp:// URLs for individual bugs and merge proposals.",
    parameters: z.object({
      op: z.enum(["resource_view", "repo_view", "file_read", "search_bugs", "search_merge_proposals"]),
      ...sharedParameters,
    }),
    approval: "read",
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      validateOperation(params)
      if (params.op === "file_read") {
        return readRepositoryFile(params, signal)
      }
      return bridgeResult(await runBridge(params, signal, ctx.cwd))
    },
  })

  pi.registerTool({
    name: "launchpad_write",
    label: "Launchpad Write",
    description:
      "Mutate Launchpad or local Git state: create bugs or merge proposals, add bug comments or merge-proposal " +
      "reviews, change merge-proposal status, check out a proposal, or push its checked-out branch.",
    parameters: z.object({
      op: z.enum([
        "bug_create",
        "merge_proposal_create",
        "comment",
        "set_merge_proposal_status",
        "merge_proposal_checkout",
        "merge_proposal_push",
      ]),
      ...sharedParameters,
    }),
    approval: "exec",
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      validateOperation(params)
      if (params.op === "merge_proposal_checkout") {
        return checkoutMergeProposal(params, signal, ctx.cwd)
      }
      if (params.op === "merge_proposal_push") {
        return pushMergeProposal(params, signal, ctx.cwd)
      }
      return bridgeResult(await runBridge(params, signal, ctx.cwd), true)
    },
  })
}

function isLaunchpadUrl(path) {
  return typeof path === "string" && path.startsWith("lp://")
}

function validateOperation(params) {
  const required = {
    resource_view: ["target"],
    repo_view: ["repository"],
    file_read: ["repository", "path"],
    search_bugs: ["target"],
    search_merge_proposals: ["repository"],
    bug_create: ["target", "title", "description"],
    merge_proposal_create: ["repository", "source_ref", "target_ref"],
    comment: ["target", "body"],
    set_merge_proposal_status: ["target", "status"],
    merge_proposal_checkout: ["target"],
  }
  for (const field of required[params.op] ?? []) {
    if (typeof params[field] !== "string" || params[field].trim() === "") {
      throw new Error(`${field} is required for ${params.op}`)
    }
  }
  if (params.limit !== undefined && (!Number.isFinite(params.limit) || params.limit <= 0)) {
    throw new Error("limit must be greater than zero")
  }
}

async function runBridge(request, signal, cwd) {
  const binary = process.env.OMP_LAUNCHPAD_LP_SHELL || Bun.which("lp-shell")
  if (!binary) {
    throw new Error("lp-shell is not installed. Install lptools and ensure lp-shell is on PATH.")
  }

  const command =
    'import os; globals()["lp"] = lp; exec(compile(open(os.environ["OMP_LAUNCHPAD_BRIDGE"], encoding="utf-8").read(), os.environ["OMP_LAUNCHPAD_BRIDGE"], "exec"), globals())'
  const args = [binary]
  if (process.env.OMP_LAUNCHPAD_ANONYMOUS === "1") {
    args.push("-a")
  }
  args.push(
    "-c",
    command,
    process.env.OMP_LAUNCHPAD_INSTANCE || "production",
    process.env.OMP_LAUNCHPAD_API_VERSION || "devel"
  )

  const child = Bun.spawn({
    cmd: args,
    cwd,
    env: {
      ...process.env,
      OMP_LAUNCHPAD_BRIDGE: BRIDGE_PATH,
      OMP_LAUNCHPAD_REQUEST: JSON.stringify(request),
    },
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    signal,
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])

  const markerIndex = stdout.lastIndexOf(RESULT_MARKER)
  if (markerIndex < 0) {
    const message = stderr.trim() || stdout.trim() || `lp-shell exited with status ${exitCode}`
    throw new Error(message)
  }
  let payload
  try {
    payload = JSON.parse(stdout.slice(markerIndex + RESULT_MARKER.length).trim())
  } catch (error) {
    throw new Error(`lp-shell returned invalid bridge output: ${error.message}`)
  }
  if (!payload.ok) {
    throw new Error(payload.error || "Launchpad operation failed")
  }
  return payload
}

function bridgeResult(payload, changed = false) {
  return {
    content: [{ type: "text", text: payload.text }],
    sourceUrl: payload.source_url || undefined,
    details: {
      ...(payload.details || {}),
      changed,
      provider: "launchpad",
    },
  }
}

async function checkoutMergeProposal(params, signal, cwd) {
  const payload = await runBridge(
    { op: "resource_view", target: withCommentsDisabled(params.target) },
    signal,
    cwd
  )
  const details = payload.details || {}
  if (details.kind !== "branch_merge_proposal") {
    throw new Error("target is not a Launchpad merge proposal")
  }
  let sourceUrl = details.source_https_url || details.source_ssh_url
  const targetUrl = details.target_https_url || details.target_ssh_url
  const sourceRef = details.source_ref
  if (!sourceUrl || !sourceRef || !details.id) {
    throw new Error("Launchpad did not return complete source repository metadata")
  }
  const branch = sourceRef.replace(/^refs\/heads\//, "")
  const repositoryName = basename(details.source_repository || "repository")
  const fallback = join(homedir(), ".omp", "wt", `lp-${details.id}-${repositoryName}`)
  const destination = params.directory
    ? isAbsolute(params.directory)
      ? params.directory
      : resolve(cwd, params.directory)
    : fallback
  if (existsSync(destination)) {
    throw new Error(`Checkout destination already exists: ${destination}`)
  }

  try {
    await runCommand("git", ["clone", "--branch", branch, "--single-branch", sourceUrl, destination], {
      cwd,
      signal,
    })
  } catch (error) {
    const fallbackUrl = details.source_ssh_url
    if (!fallbackUrl || fallbackUrl === sourceUrl) {
      throw error
    }
    rmSync(destination, { recursive: true, force: true })
    sourceUrl = fallbackUrl
    await runCommand("git", ["clone", "--branch", branch, "--single-branch", sourceUrl, destination], {
      cwd,
      signal,
    })
  }
  if (targetUrl && targetUrl !== sourceUrl) {
    await runCommand("git", ["-C", destination, "remote", "add", "upstream", targetUrl], {
      cwd,
      signal,
    })
  }
  return {
    content: [
      {
        type: "text",
        text: `# Checked out Launchpad merge proposal ${details.id}\n\n- **Branch:** ${branch}\n- **Path:** ${destination}\n- **Origin:** ${sourceUrl}${targetUrl && targetUrl !== sourceUrl ? `\n- **Upstream:** ${targetUrl}` : ""}`,
      },
    ],
    sourceUrl: payload.source_url || undefined,
    details: {
      provider: "launchpad",
      kind: "checkout",
      changed: true,
      proposalId: details.id,
      branch,
      directory: destination,
      origin: sourceUrl,
      upstream: targetUrl && targetUrl !== sourceUrl ? targetUrl : null,
    },
  }
}

async function pushMergeProposal(params, signal, cwd) {
  const directory = params.directory
    ? isAbsolute(params.directory)
      ? params.directory
      : resolve(cwd, params.directory)
    : cwd
  const branch = (
    await runCommand("git", ["-C", directory, "rev-parse", "--abbrev-ref", "HEAD"], {
      cwd,
      signal,
    })
  ).stdout.trim()
  if (!branch || branch === "HEAD") {
    throw new Error("Cannot push a detached HEAD")
  }
  const args = ["-C", directory, "push"]
  if (params.force_with_lease) {
    args.push("--force-with-lease")
  }
  args.push("origin", "HEAD")
  const result = await runCommand("git", args, { cwd, signal })
  return {
    content: [
      {
        type: "text",
        text: `# Pushed Launchpad merge proposal branch\n\n- **Branch:** ${branch}\n- **Path:** ${directory}${result.stderr.trim() ? `\n\n${result.stderr.trim()}` : ""}`,
      },
    ],
    details: {
      provider: "launchpad",
      kind: "push",
      changed: true,
      branch,
      directory,
      forceWithLease: params.force_with_lease === true,
    },
  }
}

function withCommentsDisabled(target) {
  const separator = target.includes("?") ? "&" : "?"
  return `${target}${separator}comments=0`
}

async function runCommand(command, args, { cwd, signal }) {
  const child = Bun.spawn({
    cmd: [command, ...args],
    cwd,
    env: process.env,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    signal,
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  if (exitCode !== 0) {
    throw new Error(stderr.trim() || stdout.trim() || `${command} exited with status ${exitCode}`)
  }
  return { stdout, stderr }
}

async function readRepositoryFile(params, signal) {
  const path = params.path.trim()
  if (path.startsWith("/") || path.split("/").includes("..")) {
    throw new Error("path must be repository-relative and cannot contain '..'")
  }
  const repository = params.repository.trim().replace(/^\/+|\/+$/g, "")
  const encodedRepository = repository.split("/").map(encodeURIComponent).join("/")
  const encodedPath = path.split("/").map(encodeURIComponent).join("/")
  const url = new URL(`https://git.launchpad.net/${encodedRepository}/plain/${encodedPath}`)
  if (params.branch) {
    url.searchParams.set("h", params.branch)
  }
  const response = await fetch(url, { signal, redirect: "follow" })
  if (!response.ok) {
    throw new Error(`Launchpad file read failed: HTTP ${response.status} ${response.statusText}`)
  }
  const bytes = new Uint8Array(await response.arrayBuffer())
  if (bytes.byteLength > MAX_FILE_BYTES) {
    throw new Error(`Launchpad file is larger than ${MAX_FILE_BYTES} bytes`)
  }
  const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes)
  return {
    content: [{ type: "text", text }],
    sourceUrl: url.toString(),
    details: {
      provider: "launchpad",
      kind: "file",
      repository,
      path,
      branch: params.branch || null,
      bytes: bytes.byteLength,
      changed: false,
    },
  }
}
